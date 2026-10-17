import assert from 'node:assert/strict'
import { access, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises'
import { homedir, tmpdir } from 'node:os'
import { join } from 'node:path'
import { spawnSync } from 'node:child_process'
import test from 'node:test'
import { Context } from '@deepseek-ai/cordis'
import ToolRuntime from '@deepseek-ai/dsh-tools'
import SystemPrompt from '@deepseek-ai/dsh-system-prompt'
import LocalSubprocess from '@deepseek-ai/dsh-subprocess-local'
import SandboxPolicy from '@deepseek-ai/dsh-sandbox-policy'
import LocalSandbox from '@deepseek-ai/dsh-sandbox-local'
import { ShellEnvRegistry } from '@deepseek-ai/dsh-shell-env'
import { SessionStore } from '@deepseek-ai/dsh-session'
import { createScope } from '@deepseek-ai/dsh-scope'
import { setSandboxMode } from '@deepseek-ai/dsh-sandbox-policy'
import * as facade from '../facade.js'
import * as disclosure from '../index.js'

const binary = process.env.PROG_TEST_BINARY

function cli(dir, args) {
  const result = spawnSync(binary, [`--dir=${join(dir, '.prog')}`, ...args], {
    cwd: dir, encoding: 'utf8', timeout: 10000,
  })
  assert.ifError(result.error)
  assert.equal(result.status, 0, result.stdout + result.stderr)
  return JSON.parse(result.stdout)
}

function alive(pid) {
  const result = spawnSync('ps', ['-o', 'stat=', '-p', String(pid)], { encoding: 'utf8' })
  assert.ifError(result.error)
  if (result.status === 1 && !result.stdout.trim()) return false
  assert.equal(result.status, 0, result.stderr)
  return !result.stdout.trim().startsWith('Z')
}

async function started(path) {
  for (let i = 0; i < 300; i++) {
    try { return Number(await readFile(path, 'utf8')) } catch { await new Promise(resolve => setTimeout(resolve, 10)) }
  }
  assert.fail('source did not publish its pid')
}

async function within(promise, ms = 8000) {
  let timer
  try {
    return await Promise.race([promise, new Promise((_resolve, reject) => {
      timer = setTimeout(() => reject(new Error('independent facade test deadline')), ms)
    })])
  } finally { clearTimeout(timer) }
}

async function host(t, config = {}, policy = {}, installedPlugin = facade) {
  const dir = await mkdtemp(join(tmpdir(), "prog-facade 'fixture-"))
  const ctx = new Context()
  const fibers = []
  const scopes = []
  const spawns = []
  const scriptInterpreters = new Map()
  class RecordedSubprocess extends LocalSubprocess {
    spawn(spec) {
      const interpreter = scriptInterpreters.get(spec.argv[0])
      const argv = interpreter ? [interpreter, ...spec.argv] : spec.argv
      spawns.push({ argv: [...argv], cwd: spec.cwd })
      return super.spawn({ ...spec, argv })
    }
  }
  t.after(async () => {
    for (const scope of scopes.reverse()) await scope.dispose()
    for (const fiber of fibers.reverse()) await fiber.dispose()
    await rm(dir, { recursive: true, force: true })
  })
  for (const [plugin, options] of [
    [SystemPrompt, { includeHarnessIdentity: false }], [ToolRuntime, {}],
    [RecordedSubprocess, {}], [SessionStore, {}],
    [SandboxPolicy, { mode: 'danger-full-access', workspaceRoot: dir, ...policy }],
    [LocalSandbox, {}], [ShellEnvRegistry, { dshHome: join(dir, 'harness') }], [installedPlugin, { progCommand: binary, ...config }],
  ]) fibers.push(await ctx.plugin(plugin, options))
  let sequence = 0
  return {
    ctx, dir, fibers, spawns, scriptInterpreters,
    sessionAgent(cwd) {
      // A protocol caller carrying a real host session/scope; no model driver
      // or actual-agent outcome is simulated or invoked by this fixture.
      const session = ctx.sessions.create(undefined, { meta: { cwd } })
      const agent = { id: session.id, session }
      const scope = createScope(ctx, agent)
      agent.ctx = scope.ctx
      scopes.push(scope)
      return agent
    },
    call: (name, args, signal = new AbortController().signal, agent) => ctx.tools.execute({
      callId: `fixture-${++sequence}`, name, arguments: args, signal, ...(agent ? { agent } : {}),
    }),
  }
}

test('real registry assembles three tools and captures through the host subprocess', { skip: !binary }, async t => {
  const source = await host(t)
  assert.deepEqual(source.ctx.tools.schemas().map(tool => tool.name), ['prog_observe', 'prog_evidence', 'prog_status'])
  const assembly = await source.ctx.systemPrompt.assemble()
  assert.deepEqual(assembly.tools.map(tool => tool.name), ['prog_evidence', 'prog_observe', 'prog_status'])
  assert.ok(assembly.sections.some(section => section.text === facade.instructions))
  const result = await source.call('prog_observe', { mode: 'text', text: '{"answer":42}', mime: 'application/json' })
  assert.equal(result.isError, false, JSON.stringify(result))
  assert.equal(result.value.schema, 'prog.disclosure')
  const evidence = await source.call('prog_evidence', { mode: 'exact', cursor: result.value.cursor, path: '/answer' })
  assert.equal(evidence.isError, false, JSON.stringify(evidence))
  assert.equal(evidence.value.excerpt, 42)
  const status = await source.call('prog_status', {})
  assert.equal(status.isError, false, JSON.stringify(status))
  assert.equal(status.value.schema, 'prog.status')
  const exact = await source.call('prog_evidence', { mode: 'exact', reference: evidence.value.evidence_ref })
  assert.equal(exact.value.excerpt, 42)
  await writeFile(join(source.dir, 'artifact.json'), '{"item":"fixture needle"}')
  const file = await source.call('prog_observe', { mode: 'file', file: 'artifact.json' })
  assert.equal(file.isError, false, JSON.stringify(file))
  const search = await source.call('prog_evidence', { mode: 'search', cursor: file.value.cursor, query: 'needle', limit: 1 })
  assert.equal(search.isError, false, JSON.stringify(search))
  assert.equal(search.value.hits.length, 1)
  const canonical = cli(source.dir, ['search', file.value.cursor, 'needle', '--limit', '1'])
  assert.deepEqual(search.value.hits, canonical.hits)
  assert.deepEqual(search.value.omitted, canonical.omitted)
  const altered = { ...evidence.value.evidence_ref, redacted_slice_sha256: '0'.repeat(64) }
  const mismatch = await source.call('prog_evidence', { mode: 'exact', reference: altered })
  assert.equal(mismatch.isError, true)
})

test('real host guards and pre-cancellation prevent every process launch', { skip: !binary }, async t => {
  const source = await host(t)
  const remove = source.ctx.tools.guard(() => 'fixture host denial')
  const denied = await source.call('prog_observe', { mode: 'run', argv: ['never-execute'] })
  assert.equal(denied.isError, true)
  assert.equal(source.spawns.length, 0)
  remove()
  const controller = new AbortController()
  controller.abort()
  const cancelled = await source.call('prog_observe', { mode: 'run', argv: ['never-execute'] }, controller.signal)
  assert.equal(cancelled.isError, true)
  assert.equal(source.spawns.length, 0)
  for (const args of [
    { mode: 'run', argv: ['never-execute'], tty: true },
    { mode: 'run', argv: ['never-execute'], streaming: true },
    { mode: 'run', argv: ['never-execute'], text: 'ignored?' },
    { mode: 'run', argv: ['never-execute', '\ud800'] },
  ]) assert.equal((await source.call('prog_observe', args)).isError, true)
  assert.equal(source.spawns.length, 0)
})

test('oversized host denial remains an error with a bounded final presentation', { skip: !binary }, async t => {
  const source = await host(t, { budgetBytes: 4096 })
  source.ctx.tools.guard(() => 'denied '.repeat(10000))
  const result = await source.call('prog_observe', { mode: 'run', argv: ['never-execute'] })
  assert.equal(result.isError, true)
  assert.ok(Buffer.byteLength(JSON.stringify(result.content)) < 4096)
  assert.match(result.content[0].text, /presentation is unavailable/)
  assert.equal(source.spawns.length, 0)
})

test('unavailable sandbox and executable fail before source execution', { skip: !binary }, async t => {
  const source = await host(t, {}, { mode: 'workspace-write' })
  await source.fibers[5].dispose()
  const result = await source.call('prog_observe', { mode: 'run', argv: ['never-execute'] })
  assert.equal(result.isError, true)
  assert.match(result.content[0].text, /sandbox is unavailable/)
  assert.equal(source.spawns.length, 0)
  await source.fibers.pop().dispose()
  source.fibers.push(await source.ctx.plugin(facade, { progCommand: join(source.dir, 'missing-prog') }))
  const missing = await source.call('prog_status', {})
  assert.equal(missing.isError, true)
  assert.equal(source.spawns.length, 0)
})

test('failure to prove cleanup is visible after an earlier CLI error', { skip: !binary }, async t => {
  const source = await host(t)
  const spawn = source.ctx.subprocess.spawn.bind(source.ctx.subprocess)
  source.ctx.subprocess.spawn = spec => ({ ...spawn(spec), waitForExit: async () => false })
  const result = await source.call('prog_evidence', { mode: 'exact', cursor: 'invalid' })
  assert.equal(result.isError, true)
  assert.match(result.content[0].text, /cleanup is incomplete/)
  assert.equal(result.error.info.code, 'PROG_CLEANUP_INCOMPLETE')
})

test('source children use scrubbed ambient settings and fresh managed host identity', { skip: !binary }, async t => {
  const source = await host(t)
  const names = ['OPENAI_API_KEY', 'DSH_FACADE_FIXTURE', 'DSH_FACADE_UNOWNED']
  const before = Object.fromEntries(names.map(name => [name, process.env[name]]))
  for (const name of names) process.env[name] = 'synthetic-ambient-fixture'
  t.after(() => { for (const name of names) {
    if (before[name] === undefined) delete process.env[name]; else process.env[name] = before[name]
  } })
  let revision = 'first'
  const dispose = source.ctx.shellEnv.register({ name: 'facade-fixture',
    variables: { DSH_FACADE_FIXTURE: { description: 'Synthetic managed fixture identity' } },
    resolve: () => ({ DSH_FACADE_FIXTURE: revision }),
  })
  t.after(dispose)
  const script = join(source.dir, 'environment.mjs')
  await writeFile(script, `console.log(JSON.stringify({providerSeen:Object.hasOwn(process.env,'OPENAI_API_KEY'),unownedPresent:Object.hasOwn(process.env,'DSH_FACADE_UNOWNED'),identity:process.env.DSH_FACADE_FIXTURE}))`)
  for (const current of ['first', 'second']) {
    revision = current
    const result = await source.call('prog_observe', { mode: 'run', argv: [process.execPath, script] })
    assert.equal(result.isError, false, JSON.stringify(result))
    const evidence = await source.call('prog_evidence', { mode: 'exact', cursor: result.value.cursor, path: '/stdout/text' })
    assert.deepEqual(JSON.parse(evidence.value.excerpt), { providerSeen: false, unownedPresent: false, identity: current })
  }
  for (const name of names) assert.equal(process.env[name], 'synthetic-ambient-fixture')
})

test('owner opt-in registers and disposes tools without recapturing facade errors', { skip: !binary }, async t => {
  const source = await host(t, { facade: false }, {}, disclosure)
  assert.deepEqual(source.ctx.tools.schemas(), [])
  await source.fibers.pop().dispose()
  const marker = join(source.dir, 'unexpected-post-capture')
  const helper = join(source.dir, 'helper.mjs')
  await writeFile(helper, `import {writeFileSync} from 'node:fs'; writeFileSync(${JSON.stringify(marker)}, 'bad'); process.stdin.resume()`)
  const config = { progCommand: process.execPath, progArgs: [helper], minBytes: 1, facade: { progCommand: binary } }
  for (let installation = 0; installation < 2; installation++) {
    const fiber = await source.ctx.plugin(disclosure, config)
    source.fibers.push(fiber)
    assert.equal(source.ctx.tools.schemas().length, 3)
    const error = await source.call('prog_evidence', { mode: 'exact', cursor: 'invalid-cursor', path: '' })
    assert.equal(error.isError, true)
    await assert.rejects(access(marker), { code: 'ENOENT' })
    await source.fibers.pop().dispose()
    assert.deepEqual(source.ctx.tools.schemas(), [])
    assert.ok(!(await source.ctx.systemPrompt.assemble()).sections.some(section => section.text === facade.instructions))
  }
})

test('MCP received errors retain their canonical failure observation and exact evidence', { skip: !binary }, async t => {
  const source = await host(t)
  const script = join(source.dir, 'error-server.py')
  await writeFile(script, `import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if 'id' not in m: continue
    if m['method'] == 'initialize':
        value = {'protocolVersion':m['params']['protocolVersion'], 'capabilities':{'tools':{}}, 'serverInfo':{'name':'fixture','version':'1'}}
    elif m['method'] == 'tools/list':
        value = {'tools':[{'name':'read','inputSchema':{'type':'object'},'annotations':{'readOnlyHint':True}}]}
    else:
        value = {'isError':True,'content':[{'type':'text','text':'fixture upstream failure'}]}
    print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':value}), flush=True)
`)
  const seed = join(source.dir, 'seed.json')
  await writeFile(seed, JSON.stringify({ command: 'python3', args: [script], timeout_ms: 3000 }))
  cli(source.dir, ['discover', 'failing', '--kind', 'mcp', '--seed', seed])
  const result = await source.call('prog_observe', { mode: 'call', source: 'failing', operation: 'read', arguments: {} })
  assert.equal(result.isError, false, JSON.stringify(result))
  assert.equal(result.value.schema, 'prog.disclosure')
  assert.ok(result.additionalContexts.some(context => context.content.some(block => block.text?.includes('error response'))))
  const evidence = await source.call('prog_evidence', { mode: 'exact', cursor: result.value.cursor })
  assert.equal(evidence.isError, false, JSON.stringify(evidence))
  assert.match(JSON.stringify(evidence.value.excerpt), /fixture upstream failure/)
  assert.equal(result.value.provenance.received_error, true)
  assert.equal(source.spawns.filter(row => row.argv.includes('call')).length, 1)
})

test('argv, relative cwd, exit/signal facts and ordinary environment survive the real host', { skip: !binary }, async t => {
  const source = await host(t)
  const project = join(source.dir, 'session workspace')
  const subdir = join(project, 'child cwd')
  await mkdir(subdir, { recursive: true })
  const agent = source.sessionAgent(project)
  const envName = 'PROG_FACADE_FIXTURE'
  const before = process.env[envName]
  process.env[envName] = "ordinary value 'with spaces'"
  t.after(() => { if (before === undefined) delete process.env[envName]; else process.env[envName] = before })
  const script = join(project, 'source.mjs')
  await writeFile(script, `process.stdout.write(JSON.stringify({argv:process.argv.slice(2),cwd:process.cwd(),env:process.env.PROG_FACADE_FIXTURE})); process.stderr.write('stderr evidence'); process.exit(7)`)
  const tail = ['literal $(false) `false`', 'with spaces', '', '--flag']
  const argv = [process.execPath, script, ...tail]
  const result = await source.call('prog_observe', { mode: 'run', argv, workdir: 'child cwd' }, undefined, agent)
  assert.equal(result.isError, false, JSON.stringify(result))
  const evidence = await source.call('prog_evidence', { mode: 'exact', cursor: result.value.cursor, path: '/command' }, undefined, agent)
  assert.equal(evidence.value.excerpt.exit_code, 7)
  const args = await source.call('prog_evidence', { mode: 'expand', cursor: result.value.cursor, path: '/command/argv', limit: 100 }, undefined, agent)
  assert.deepEqual(args.value.data_preview, argv)
  const out = await source.call('prog_evidence', { mode: 'exact', cursor: result.value.cursor, path: '/stdout/text' }, undefined, agent)
  assert.deepEqual(JSON.parse(out.value.excerpt), { argv: tail, cwd: await realpath(subdir), env: process.env[envName] })
  const err = await source.call('prog_evidence', { mode: 'exact', cursor: result.value.cursor, path: '/stderr/text' }, undefined, agent)
  assert.equal(err.value.excerpt, 'stderr evidence')
  assert.equal(source.spawns.filter(row => row.argv.includes('run')).length, 1)
  const signalled = await source.call('prog_observe', { mode: 'run', argv: [process.execPath, '-e', "process.kill(process.pid, 'SIGTERM')"] }, undefined, agent)
  assert.equal(signalled.isError, false, JSON.stringify(signalled))
  const facts = await source.call('prog_evidence', { mode: 'exact', cursor: signalled.value.cursor, path: '/command' }, undefined, agent)
  assert.equal(facts.value.excerpt.signal, 15)
  assert.equal(facts.value.excerpt.exit_code, null)
})

test('the host sandbox confines the entire prog invocation and observes session policy changes', { skip: !binary }, async t => {
  const source = await host(t)
  const project = join(source.dir, 'workspace')
  await mkdir(project)
  // The host intentionally permits its temporary roots. Use a disposable
  // fixture under HOME to exercise a genuinely excluded write location.
  const excluded = await mkdtemp(join(homedir(), '.prog-facade-denial-'))
  t.after(() => rm(excluded, { recursive: true, force: true }))
  const outside = join(excluded, 'outside')
  await writeFile(outside, 'original')
  const agent = source.sessionAgent(project)
  setSandboxMode(agent.session, 'workspace-write')
  const argv = [process.execPath, '-e', "require('node:fs').writeFileSync(process.argv[1], 'changed')", outside]
  const result = await source.call('prog_observe', { mode: 'run', argv }, undefined, agent)
  assert.equal(result.isError, false, JSON.stringify(result))
  assert.equal(await readFile(outside, 'utf8'), 'original')
  const facts = await source.call('prog_evidence', { mode: 'exact', cursor: result.value.cursor, path: '/command' }, undefined, agent)
  assert.notEqual(facts.value.excerpt.exit_code, 0)
  assert.notEqual(source.spawns[0].argv[0], await realpath(binary))
  assert.ok(result.additionalContexts?.some(context => context.content.some(block => block.text?.includes('enforcement'))))
  setSandboxMode(agent.session, 'danger-full-access')
  const allowed = await source.call('prog_observe', { mode: 'run', argv }, undefined, agent)
  assert.equal(allowed.isError, false, JSON.stringify(allowed))
  assert.equal(await readFile(outside, 'utf8'), 'changed')
})

test('configured source calls retain confirmation and source-profile trust gates', { skip: !binary }, async t => {
  const source = await host(t)
  const marker = join(source.dir, 'mutated')
  const script = join(source.dir, 'writer.mjs')
  await writeFile(script, `import {writeFileSync} from 'node:fs'; writeFileSync(${JSON.stringify(marker)}, 'once'); console.log('{"ok":true}')`)
  cli(source.dir, ['source', 'add-cli', 'writer', '--operation', 'write', '--', process.execPath, script])
  const request = { mode: 'call', source: 'writer', operation: 'write', arguments: {} }
  const denied = await source.call('prog_observe', request)
  assert.equal(denied.isError, true)
  await assert.rejects(access(marker), { code: 'ENOENT' })
  const permitted = await source.call('prog_observe', { ...request, yes: true })
  assert.equal(permitted.isError, false, JSON.stringify(permitted))
  assert.equal(await readFile(marker, 'utf8'), 'once')
  const shell = join(source.dir, 'source.sh')
  await writeFile(shell, 'printf \'{"ok":true}\\n\'\n')
  cli(source.dir, ['source', 'add-cli', 'shell', '--operation', 'read', '--read-only', '--', 'sh', shell])
  const profilePath = join(source.dir, '.prog', 'profiles', 'shell.json')
  const configured = JSON.parse(await readFile(profilePath, 'utf8'))
  configured.operations[0].effects.shell = true
  configured.trust.auto_upgrade = false
  await writeFile(profilePath, JSON.stringify(configured))
  const profile = await readFile(profilePath, 'utf8')
  const untrusted = await source.call('prog_observe', { mode: 'call', source: 'shell', operation: 'read', arguments: {}, yes: true })
  assert.equal(untrusted.isError, true)
  assert.ok(untrusted.content.some(block => block.text?.includes('shell_not_trusted')))
  assert.equal(await readFile(profilePath, 'utf8'), profile)
})

test('cancelling a configured CLI source stops its separately owned process group', { skip: !binary }, async t => {
  const source = await host(t, { graceMs: 1000 })
  const marker = join(source.dir, 'call-started')
  const late = join(source.dir, 'call-continued')
  const script = join(source.dir, 'slow-source.mjs')
  await writeFile(script, `import {writeFileSync} from 'node:fs'; writeFileSync(${JSON.stringify(marker)}, String(process.pid)); setTimeout(() => { writeFileSync(${JSON.stringify(late)}, 'bad'); console.log('{}') }, 2000)`)
  cli(source.dir, ['source', 'add-cli', 'slow', '--operation', 'read', '--read-only', '--', process.execPath, script])
  const controller = new AbortController()
  const pending = source.call('prog_observe', { mode: 'call', source: 'slow', operation: 'read', arguments: {} }, controller.signal)
  const pid = await started(marker)
  // A failed regression cannot signal a reused pid: check the owned fixture
  // script identity before cleanup, and never signal after observing exit.
  t.after(() => {
    if (!alive(pid)) return
    const identity = spawnSync('ps', ['-o', 'command=', '-p', String(pid)], { encoding: 'utf8' })
    if (identity.stdout.includes(script)) process.kill(pid, 'SIGKILL')
  })
  controller.abort()
  const result = await within(pending)
  assert.equal(result.isError, true)
  assert.equal(alive(pid), false, 'configured source must stop before cancellation settles')
  await assert.rejects(access(late), { code: 'ENOENT' })
  assert.equal(source.spawns.length, 1)
})

test('caller cancellation settles after the source process has stopped', { skip: !binary }, async t => {
  const source = await host(t, { graceMs: 1000 })
  const marker = join(source.dir, 'started')
  const escaped = join(source.dir, 'continued')
  const script = `require('node:fs').writeFileSync(${JSON.stringify(marker)}, String(process.pid)); setTimeout(() => require('node:fs').writeFileSync(${JSON.stringify(escaped)}, 'bad'), 2000)`
  const controller = new AbortController()
  const pending = source.call('prog_observe', { mode: 'run', argv: [process.execPath, '-e', script] }, controller.signal)
  for (let i = 0; ; i++) {
    try { await access(marker); break } catch { assert.ok(i < 300, 'source should start'); await new Promise(resolve => setTimeout(resolve, 10)) }
  }
  const pid = Number(await readFile(marker, 'utf8'))
  t.after(() => {
    if (!alive(pid)) return
    const identity = spawnSync('ps', ['-o', 'command=', '-p', String(pid)], { encoding: 'utf8' })
    if (identity.stdout.includes(source.dir)) process.kill(pid, 'SIGKILL')
  })
  controller.abort()
  const result = await within(pending)
  assert.equal(result.isError, true)
  assert.equal(alive(pid), false)
  await assert.rejects(access(escaped), { code: 'ENOENT' })
  assert.equal(source.spawns.length, 1)
})

test('the source timeout retains partial evidence and cannot prove absence', { skip: !binary }, async t => {
  const source = await host(t)
  const script = join(source.dir, 'deadline.mjs')
  await writeFile(script, "process.stdout.write('before timeout'); process.stderr.write('stderr before timeout'); setTimeout(() => {}, 5000)")
  const result = await within(source.call('prog_observe', { mode: 'run', argv: [process.execPath, script], timeout_ms: 500 }))
  assert.equal(result.isError, false, JSON.stringify(result))
  assert.equal(result.value.observation.capture.stop_reason, 'timeout')
  assert.equal(result.value.observation.capture.can_prove_absence, false)
  const evidence = await source.call('prog_evidence', { mode: 'exact', cursor: result.value.cursor, path: '/stdout/text' })
  assert.equal(evidence.value.excerpt, 'before timeout')
  const stderr = await source.call('prog_evidence', { mode: 'exact', cursor: result.value.cursor, path: '/stderr/text' })
  assert.equal(stderr.value.excerpt, 'stderr before timeout')
})

async function transport(t, { mode, pipe = 'stdout', escaped = false }) {
  const source = await host(t, { timeoutMs: 200, graceMs: 1000 })
  await source.fibers.pop().dispose()
  const script = join(source.dir, 'transport.mjs')
  const state = join(source.dir, 'transport-pids.json')
  const value = { schema: 'prog.disclosure', fixture: 'complete response' }
  await writeFile(script, `#!${process.execPath}
import {spawn} from 'node:child_process'
import {closeSync, writeFileSync, writeSync} from 'node:fs'
const encoded = ${JSON.stringify(JSON.stringify(value))}
if (process.argv[2] === 'holder') {
  if (${JSON.stringify(mode)} === 'finite') setTimeout(() => process.stdout.write(encoded.slice(encoded.length >> 1)), 150)
  else {
    if (${JSON.stringify(mode)} === 'overflow') process[${JSON.stringify(pipe)}].write(Buffer.alloc(20000, 'x'))
    setTimeout(() => {}, 5000)
  }
} else {
  if (${JSON.stringify(mode)} === 'invalid_utf8') { writeSync(1, Buffer.from([123,34,115,99,104,101,109,97,34,58,34,112,114,111,103,46,100,105,115,99,108,111,115,117,114,101,34,44,34,120,34,58,34,255,34,125])); process.exit(0) }
  if (${JSON.stringify(mode)} === 'malformed') { console.log('not JSON'); process.exit(0) }
  writeSync(1, ${JSON.stringify(mode)} === 'finite' ? encoded.slice(0, encoded.length >> 1) : encoded)
  const child = spawn(process.execPath, [${JSON.stringify(script)}, 'holder'], {
    detached: ${JSON.stringify(escaped)}, stdio: ['ignore', ${pipe === 'stdout' ? '1' : "'ignore'"}, ${pipe === 'stderr' ? '2' : "'ignore'"}],
  })
  writeFileSync(${JSON.stringify(state)}, JSON.stringify({parent:process.pid, holder:child.pid}))
  if (${JSON.stringify(mode)} === 'input_failure') closeSync(0)
  setTimeout(() => process.exit(0), 10)
}
`, { mode: 0o755 })
  t.after(async () => {
    let ids
    try { ids = JSON.parse(await readFile(state, 'utf8')) } catch { return }
    for (const pid of Object.values(ids)) {
      if (!alive(pid)) continue
      const identity = spawnSync('ps', ['-o', 'command=', '-p', String(pid)], { encoding: 'utf8' })
      if (identity.stdout.includes(script)) process.kill(pid, 'SIGKILL')
    }
  })
  // This transport fixture exercises real host pipes/process groups. Launch
  // its script through the exact interpreter so platform-dependent shebang
  // startup cannot consume the deadline before the pipe scenario begins.
  source.scriptInterpreters.set(script, process.execPath)
  source.fibers.push(await source.ctx.plugin(facade, { progCommand: script, timeoutMs: 200, graceMs: 1000, budgetBytes: 4096 }))
  return { ...source, state, value }
}

test('host raw pipes must reach EOF independently of parent exit and host done', { skip: !binary }, async t => {
  for (const scenario of [
    { mode: 'finite' }, { mode: 'hold' }, { mode: 'hold', pipe: 'stderr' },
    { mode: 'hold', escaped: true }, { mode: 'overflow' }, { mode: 'overflow', pipe: 'stderr' },
    { mode: 'input_failure' }, { mode: 'malformed' }, { mode: 'invalid_utf8' },
  ]) await t.test(JSON.stringify(scenario), async t => {
    const source = await transport(t, scenario)
    const before = performance.now()
    const request = scenario.mode === 'input_failure'
      ? { mode: 'text', text: 'x'.repeat(2 * 1024 * 1024) }
      : { mode: 'file', file: '/fixture-unused' }
    const result = await within(source.call('prog_observe', request))
    assert.equal(source.spawns.length, 1)
    if (scenario.mode === 'finite') {
      assert.equal(result.isError, false, JSON.stringify(result))
      assert.deepEqual(result.value, source.value)
    } else {
      assert.equal(result.isError, true, JSON.stringify(result))
      assert.equal(result.value, undefined, 'a valid prefix cannot authorize capture after failure')
      assert.ok(Buffer.byteLength(JSON.stringify(result.content)) < 4300)
      assert.ok(performance.now() - before < 4000, 'original acquisition deadline remains bounded')
    }
    if (!['malformed', 'invalid_utf8'].includes(scenario.mode)) {
      const ids = JSON.parse(await readFile(source.state, 'utf8'))
      assert.equal(alive(ids.parent), false)
      assert.equal(alive(ids.holder), scenario.escaped ?? false,
        'only the owned process group is covered by host cleanup')
    }
  })
})
