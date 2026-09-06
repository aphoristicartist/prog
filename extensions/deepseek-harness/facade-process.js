import { createUserMessage, HarnessError } from '@deepseek-ai/dsh-llm'

// The host owns the execution world, environment scrub, confinement and
// process group. prog owns capture/redaction/storage inside that boundary.
export async function invokeProg(ctx, exec, config, request) {
  exec.signal.throwIfAborted()
  const policy = ctx.sandboxPolicy.resolve(exec.agent ? { session: exec.agent.session } : {})
  const cwd = request.cwd(policy.workspaceRoot)
  const env = request.shellEnvironment ? ctx.shellEnv.collect(exec) : undefined
  const controller = new AbortController()
  const signal = AbortSignal.any([exec.signal, controller.signal])
  let failure
  let proc
  const readers = []
  const boundedError = error => {
    const bytes = Buffer.from(String(error?.message ?? error))
    const marker = '\n[error message truncated]'
    const message = bytes.length <= config.budgetBytes ? bytes.toString('utf8')
      : bytes.subarray(0, config.budgetBytes - Buffer.byteLength(marker)).toString('utf8').replace(/\uFFFD$/, '') + marker
    const code = typeof error?.code === 'string' && /^[A-Z0-9_]{1,64}$/.test(error.code) ? error.code : 'PROG_HOST_ERROR'
    return new HarnessError(message, code)
  }
  const stop = error => {
    failure ??= boundedError(error)
    controller.abort()
    proc?.terminate()
    proc?.stdin?.destroy()
    proc?.stdout?.destroy()
    proc?.stderr?.destroy()
  }
  const cancelled = () => stop(new HarnessError('prog invocation cancelled; no source retry was performed', 'PROG_CANCELLED'))
  exec.signal.addEventListener('abort', cancelled, { once: true })
  const timer = setTimeout(() => stop(new HarnessError(
    'prog invocation deadline exceeded; inspect the existing store before any manual retry', 'PROG_TIMEOUT',
  )), request.timeoutMs + config.graceMs)

  const collect = async (stream, maxBytes) => {
    const chunks = []
    let bytes = 0
    try {
      for await (const chunk of stream) {
        bytes += chunk.length
        if (bytes > maxBytes) throw new HarnessError('prog transport output exceeded its byte limit', 'PROG_OUTPUT_LIMIT')
        chunks.push(chunk)
      }
      try { return new TextDecoder('utf-8', { fatal: true }).decode(Buffer.concat(chunks)) }
      catch { throw new HarnessError('prog transport returned invalid UTF-8', 'PROG_PROTOCOL_ERROR') }
    } catch (error) {
      stop(error)
      throw error
    }
  }

  try {
    const command = await ctx.subprocess.resolveExecutable(config.progCommand, env, signal)
    signal.throwIfAborted()
    const argv = [command, `--dir=${request.store(policy.workspaceRoot)}`, `--budget-bytes=${config.budgetBytes}`, ...request.args]
    const confinement = policy.mode === 'danger-full-access' ? undefined
      : ctx.get('sandbox')?.confine(argv, policy)
    if (policy.mode !== 'danger-full-access' && confinement === undefined) {
      throw new HarnessError('Host sandbox is unavailable; refusing unconfined prog execution', 'SANDBOX_UNAVAILABLE')
    }
    if (confinement) exec.deferContext(createUserMessage({
      source: { kind: 'plugin', plugin: 'prog-facade', form: 'notice', summary: 'Host sandbox policy for prog' },
      content: [{ type: 'text', text: `Host sandbox requested ${policy.mode}; the backend reports ${confinement.enforcement} enforcement.` }],
    }))
    proc = ctx.subprocess.spawn({
      argv: confinement?.argv ?? argv, cwd, env, signal,
      graceMs: config.graceMs,
      stdio: { stdin: request.input === undefined ? 'ignore' : 'pipe', stdout: 'pipe', stderr: 'pipe' },
    })
    if (!proc.stdout || !proc.stderr) throw new Error('Host subprocess did not provide the requested pipes')
    readers.push(collect(proc.stdout, config.budgetBytes + 2), collect(proc.stderr, Math.min(8192, config.budgetBytes / 4)))
    const input = request.input === undefined ? Promise.resolve() : new Promise((resolve, reject) => {
      if (!proc.stdin) { reject(new Error('Host subprocess did not provide stdin')); return }
      proc.stdin.once('error', reject)
      proc.stdin.end(request.input, error => error ? reject(error) : resolve())
    })
    // `done` can precede raw-pipe EOF in the host. Both readers and input
    // delivery must finish independently before JSON can authorize a result.
    const [outcome, stdout, stderr] = await Promise.all([proc.done, ...readers, input])
    if (failure) throw failure
    signal.throwIfAborted()
    let value
    try { value = JSON.parse(stdout) } catch { /* transport errors retain bounded stderr below */ }
    const capturedError = request.allowErrorDisclosure && outcome.exitCode === 1
      && outcome.signal === null && value?.schema === request.schema && value.provenance?.received_error === true
    if ((outcome.exitCode !== 0 || outcome.signal !== null) && !capturedError) {
      throw new HarnessError(stdout.trim() || stderr.trim()
        || `prog exited with ${outcome.exitCode ?? outcome.signal}; no source retry was performed`, 'PROG_CLI_ERROR')
    }
    if (value?.schema !== request.schema) throw new HarnessError('prog returned an unexpected response schema', 'PROG_PROTOCOL_ERROR')
    if (Buffer.byteLength(JSON.stringify(value)) > config.budgetBytes) throw new HarnessError('prog response exceeds the configured budget', 'PROG_OUTPUT_LIMIT')
    if (capturedError) exec.deferContext(createUserMessage({
      source: { kind: 'plugin', plugin: 'prog-facade', form: 'notice', summary: 'Source call returned a captured error' },
      content: [{ type: 'text', text: 'The source call returned an error response captured in this observation. Inspect its failure evidence before deciding whether to retry.' }],
    }))
    return value
  } catch (error) {
    stop(error)
    throw failure
  } finally {
    if (proc) {
      if (failure) proc.terminate()
      const quiet = await proc.waitForExit(AbortSignal.timeout(config.graceMs + 1000))
      if (!quiet) {
        const prior = failure ? ` after ${failure.code}` : ''
        // Failure to prove cleanup must remain visible even when cancellation
        // or an output error was the original reason for stopping.
        failure = boundedError(new HarnessError(`prog process cleanup is incomplete${prior}; do not retry the source automatically`, 'PROG_CLEANUP_INCOMPLETE'))
        stop(failure)
      }
      await Promise.allSettled(readers)
    }
    clearTimeout(timer)
    exec.signal.removeEventListener('abort', cancelled)
    if (failure) throw failure
  }
}
