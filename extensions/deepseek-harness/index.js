import { spawn } from 'node:child_process'

export const name = 'prog-disclosure'
export const inject = ['tools']

const DEFAULT_MIN_BYTES = 16 * 1024
const DEFAULT_BUDGET_BYTES = 16 * 1024
const DEFAULT_TIMEOUT_MS = 30_000
const MAX_CHILD_OUTPUT_BYTES = 256 * 1024
const VERDICT_RESULTS = new Set(['raw_cheaper', 'neutral', 'bounded_win', 'unavailable'])

function plainText(content) {
  let text = ''
  for (const block of content) {
    if (block?.type !== 'text' || typeof block.text !== 'string') return undefined
    text += block.text
  }
  return text
}

function positiveInteger(value, fallback, label) {
  const resolved = value ?? fallback
  if (!Number.isInteger(resolved) || resolved <= 0) {
    throw new Error(`prog-disclosure: ${label} must be a positive integer`)
  }
  return resolved
}

function looksLikeProgResult(text) {
  const trimmed = text.trimStart()
  if (!trimmed.startsWith('{')) return false
  try {
    const value = JSON.parse(trimmed)
    return typeof value?.schema === 'string' && value.schema.startsWith('prog.')
  } catch {
    return false
  }
}

function runProg({ command, prefixArgs, args, input, cwd, timeoutMs, signal }) {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(new Error('prog capture cancelled'))
      return
    }
    const deadline = performance.now() + timeoutMs
    const child = spawn(command, [...prefixArgs, ...args], {
      cwd,
      env: process.env,
      shell: false,
      // prog supports POSIX hosts. Keep capture helpers in a group separate
      // from the host so stopping them cannot signal the upstream tool.
      detached: true,
      stdio: ['pipe', 'pipe', 'pipe'],
    })
    const stdout = []
    const stderr = []
    let stdoutBytes = 0
    let stderrBytes = 0
    let settled = false
    let timer

    const cleanup = () => {
      clearTimeout(timer)
      signal?.removeEventListener?.('abort', abort)
    }
    const stop = error => {
      if (settled) return
      settled = true
      cleanup()
      // A parent exit does not imply EOF: descendants can retain either pipe.
      // Signal the owned group even after its leader exits, then close our
      // descriptors independently of whether all descendants can be stopped.
      if (child.pid !== undefined) {
        try {
          process.kill(-child.pid, 'SIGKILL')
        } catch (killError) {
          if (killError.code !== 'ESRCH') {
            error = new Error(`${error.message}; capture group termination failed: ${killError.message}`)
          }
        }
      }
      child.stdin.destroy()
      child.stdout.destroy()
      child.stderr.destroy()
      child.unref()
      stdout.length = 0
      stderr.length = 0
      // Never depend on `close` to settle a stop, and never let a later clean
      // close turn a timed-out/cancelled prefix into successful capture.
      reject(error)
    }
    const abort = () => stop(new Error('prog capture cancelled'))
    const expired = () => stop(new Error('prog capture timed out'))
    const stopped = () => {
      if (!settled && signal?.aborted) abort()
      if (!settled && performance.now() >= deadline) expired()
      return settled
    }

    const collect = (target, chunk, stream) => {
      if (stopped()) return
      const next = stream === 'stdout' ? stdoutBytes + chunk.length : stderrBytes + chunk.length
      if (next > MAX_CHILD_OUTPUT_BYTES) {
        stop(new Error(`prog child ${stream} exceeded the adapter limit`))
        return
      }
      target.push(chunk)
      if (stream === 'stdout') stdoutBytes = next
      else stderrBytes = next
    }
    child.stdout.on('data', chunk => collect(stdout, chunk, 'stdout'))
    child.stderr.on('data', chunk => collect(stderr, chunk, 'stderr'))

    child.on('error', stop)
    child.on('close', (code, signal) => {
      if (stopped()) return
      settled = true
      cleanup()
      const stderrText = Buffer.concat(stderr).toString('utf8').trim()
      if (code !== 0) {
        reject(new Error(`prog exited with ${code ?? signal ?? 'unknown'}${stderrText ? `: ${stderrText}` : ''}`))
      } else {
        resolve(Buffer.concat(stdout).toString('utf8'))
      }
    })
    // Failed input delivery cannot authorize a valid-looking output prefix.
    // Stream failures cannot leave a reader alive until an inherited EOF.
    child.stdin.on('error', stop)
    child.stdout.on('error', stop)
    child.stderr.on('error', stop)
    signal?.addEventListener?.('abort', abort, { once: true })
    timer = setTimeout(expired, Math.max(1, Math.ceil(deadline - performance.now())))
    if (!stopped()) child.stdin.end(input)
  })
}

async function capture(text, exec, config) {
  const budgetBytes = positiveInteger(config.budgetBytes, DEFAULT_BUDGET_BYTES, 'budgetBytes')
  const timeoutMs = positiveInteger(config.timeoutMs, DEFAULT_TIMEOUT_MS, 'timeoutMs')
  const command = config.progCommand ?? 'prog'
  const prefixArgs = Array.isArray(config.progArgs) ? config.progArgs.map(String) : []
  const args = [
    ...(config.storeDir ? ['--dir', String(config.storeDir)] : []),
    '--budget-bytes', String(budgetBytes),
    'observe', '--stdin', '--mime', 'text/plain',
    '--name', `harness:${exec.name}`,
    '--comparison-family', `harness-tool:${exec.name}`,
  ]
  const output = await runProg({
    command,
    prefixArgs,
    args,
    input: text,
    cwd: config.cwd ?? process.cwd(),
    timeoutMs,
    signal: exec.signal,
  })
  const envelope = JSON.parse(output)
  if (envelope?.schema !== 'prog.disclosure'
    || typeof envelope?.cursor !== 'string'
    || !envelope.cursor.startsWith('pc1_')
    || !VERDICT_RESULTS.has(envelope?.disclosure_verdict?.result)) {
    throw new Error('prog returned no reusable disclosure cursor')
  }
  if (Buffer.byteLength(JSON.stringify(envelope), 'utf8') > budgetBytes) {
    throw new Error('prog returned an envelope larger than the configured budget')
  }
  return envelope
}

function shouldReplace(envelope) {
  const redacted = envelope?.observation?.safety?.redacted_before_persistence === true
  return redacted || envelope?.disclosure_verdict?.result !== 'raw_cheaper'
}

export function apply(ctx, config = {}) {
  const minBytes = positiveInteger(config.minBytes, DEFAULT_MIN_BYTES, 'minBytes')

  ctx.on('tools/post-execute', async (exec, result, next) => {
    const decision = await next()
    if (decision.kind !== 'accept' || Object.hasOwn(decision, 'value') || exec.parent !== undefined) {
      return decision
    }
    const content = decision.content ?? result.content
    const text = plainText(content)
    if (text === undefined || looksLikeProgResult(text)) return decision
    const originalBytes = Buffer.byteLength(text, 'utf8')
    if (originalBytes < minBytes) return decision

    try {
      const envelope = await capture(text, exec, config)
      if (!shouldReplace(envelope)) return decision
      const replacement = JSON.stringify(envelope)
      if (Buffer.byteLength(replacement, 'utf8') >= originalBytes
        && envelope?.observation?.safety?.redacted_before_persistence !== true) {
        return decision
      }
      return {
        kind: 'accept',
        content: [{ type: 'text', text: replacement }],
        ...(decision.additionalContexts ? { additionalContexts: decision.additionalContexts } : {}),
      }
    } catch (error) {
      ctx.logger?.warn?.(`prog-disclosure: ${String(error)}; keeping the original tool result`)
      return decision
    }
  }, { prepend: true })
}

export const testing = { looksLikeProgResult, plainText, shouldReplace }
