import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { spawnSync } from 'node:child_process'
import { getEventListeners } from 'node:events'
import { setTimeout as delay } from 'node:timers/promises'
import test from 'node:test'

import { apply } from '../index.js'

const envelope = {
  schema: 'prog.disclosure', cursor: 'pc1_lifecycle',
  disclosure_verdict: { result: 'bounded_win' },
  observation: { safety: { redacted_before_persistence: false } },
}

function harness(config) {
  let listener
  const warnings = []
  let calls = 0
  const accepted = { kind: 'accept', additionalContexts: [{ type: 'text', text: 'host context' }] }
  apply({
    on: (_event, callback) => { listener = callback },
    logger: { warn: message => warnings.push(message) },
  }, { minBytes: 1, budgetBytes: 4096, ...config })
  return {
    accepted, warnings,
    get calls() { return calls },
    invoke: signal => listener({ name: 'fixture-tool', signal },
      { content: [{ type: 'text', text: 'original tool result\n'.repeat(200) }], isError: false },
      async () => { calls += 1; return accepted }),
  }
}

async function within(promise, ms = 5000) {
  let timer
  try {
    return await Promise.race([promise, new Promise((_resolve, reject) => {
      timer = setTimeout(() => reject(new Error('independent test guard expired')), ms)
    })])
  } finally {
    clearTimeout(timer)
  }
}

async function eventually(check) {
  const deadline = Date.now() + 3000
  while (!await check()) {
    assert.ok(Date.now() < deadline, 'fixture condition did not become true')
    await delay(10)
  }
}

function running(pid) {
  // An orphan may be a zombie until the OS/container init reaps it. That
  // process is already stopped and cannot retain a pipe or execute more work.
  const state = spawnSync('ps', ['-o', 'stat=', '-p', String(pid)], { encoding: 'utf8' })
  assert.ifError(state.error)
  if (state.status === 1 && state.stdout.trim() === '') return false
  assert.equal(state.status, 0, state.stderr)
  return !state.stdout.trim().startsWith('Z')
}

async function fixture(t, { mode = 'hold', pipe = 'stdout', escaped = false, parentDelay = 0 } = {}) {
  const dir = await mkdtemp(join(tmpdir(), 'prog-dsh-lifecycle-'))
  const script = join(dir, 'capture.mjs')
  const stateFile = join(dir, 'processes.json')
  const startedFile = join(dir, 'started')
  const exitedFile = join(dir, 'exited')
  await writeFile(script, `
    import { spawn } from 'node:child_process'
    import { writeFileSync, writeSync } from 'node:fs'
    const encoded = ${JSON.stringify(JSON.stringify(envelope))}
    if (process.argv[2] === 'descendant') {
      if (${JSON.stringify(mode)} === 'finite') {
        setTimeout(() => process.stdout.write(encoded.slice(encoded.length >> 1)), 100)
      } else {
        if (${JSON.stringify(mode)} === 'overflow') process[${JSON.stringify(pipe)}].write(Buffer.alloc(300 * 1024, 'x'))
        // Secondary fixture bound, independent of the adapter and test guard.
        setTimeout(() => {}, 10000)
      }
    } else {
      writeFileSync(${JSON.stringify(startedFile)}, 'started')
      writeSync(1, ${JSON.stringify(mode)} === 'finite' ? encoded.slice(0, encoded.length >> 1) : encoded)
      const child = spawn(process.execPath, [${JSON.stringify(script)}, 'descendant'], {
        detached: ${JSON.stringify(escaped)},
        stdio: ['ignore', ${pipe === 'stdout' ? '1' : "'ignore'"}, ${pipe === 'stderr' ? '2' : "'ignore'"}],
      })
      writeFileSync(${JSON.stringify(stateFile)}, JSON.stringify({ parent: process.pid, descendant: child.pid }))
      setTimeout(() => {
        writeFileSync(${JSON.stringify(exitedFile)}, 'exited')
        process.exit(0)
      }, ${parentDelay})
    }
  `)
  t.after(async () => {
    // Cleanup runs even against the old hanging implementation. Only fixture
    // PIDs are signalled; no broad process-name or process-tree kill is used.
    if (existsSync(stateFile)) {
      const state = JSON.parse(await readFile(stateFile, 'utf8'))
      for (const pid of [state.parent, state.descendant]) {
        if (running(pid)) {
          try { process.kill(pid, 'SIGKILL') } catch (error) {
            if (error.code !== 'ESRCH') throw error
          }
        }
      }
      await eventually(() => !running(state.descendant))
    }
    await rm(dir, { recursive: true, force: true })
  })
  return {
    config: { progCommand: process.execPath, progArgs: [script], timeoutMs: 1000 },
    startedFile, exitedFile,
    exitedParent: async () => {
      await eventually(() => existsSync(stateFile))
      const state = JSON.parse(await readFile(stateFile, 'utf8'))
      await eventually(() => !running(state.parent))
      return state
    },
  }
}

for (const pipe of ['stdout', 'stderr']) {
  for (const cancel of [false, true]) {
    test(`${cancel ? 'cancellation' : 'deadline'} after parent exit stops inherited ${pipe} and rejects a valid prefix`, async t => {
      const source = await fixture(t, { pipe })
      const capture = harness({ ...source.config, timeoutMs: cancel ? 5000 : 1000 })
      const controller = new AbortController()
      const started = Date.now()
      const pending = capture.invoke(controller.signal)
      const state = await source.exitedParent()
      assert.ok(running(state.descendant), 'descendant must retain the pipe after parent exit')
      const cancelAt = Date.now()
      if (cancel) controller.abort()
      const actual = await within(pending)
      assert.equal(actual, capture.accepted, 'a valid prefix cannot become a replacement after stopping')
      assert.equal(capture.calls, 1)
      assert.equal(capture.warnings.length, 1)
      assert.match(capture.warnings[0], cancel ? /cancelled/ : /timed out/)
      assert.equal(getEventListeners(controller.signal, 'abort').length, 0)
      assert.ok(Date.now() - (cancel ? cancelAt : started) < (cancel ? 2000 : 3000))
      await eventually(() => !running(state.descendant))
    })
  }

  test(`descendant ${pipe} overflow stops capture without waiting for its deadline`, async t => {
    const source = await fixture(t, { mode: 'overflow', pipe })
    const capture = harness({ ...source.config, timeoutMs: 10000 })
    const started = Date.now()
    const actual = await within(capture.invoke())
    const state = await source.exitedParent()
    assert.equal(actual, capture.accepted)
    assert.equal(capture.calls, 1)
    assert.equal(capture.warnings.length, 1)
    assert.match(capture.warnings[0], new RegExp(`${pipe} exceeded the adapter limit`))
    assert.ok(Date.now() - started < 5000)
    await eventually(() => !running(state.descendant))
  })
}

test('escaped descendant cannot retain the adapter pipes or prevent cancellation fallback', async t => {
  const source = await fixture(t, { escaped: true })
  const capture = harness({ ...source.config, timeoutMs: 10000 })
  const controller = new AbortController()
  const pending = capture.invoke(controller.signal)
  const state = await source.exitedParent()
  assert.ok(running(state.descendant))
  controller.abort()
  assert.equal(await within(pending), capture.accepted)
  assert.equal(capture.calls, 1)
  assert.match(capture.warnings[0], /cancelled/)
  assert.ok(running(state.descendant), 'an escaped group is outside the adapter termination guarantee')
  // The fixture teardown owns and terminates this deliberately escaped process.
})

test('an already-aborted capture starts no helper', async t => {
  const source = await fixture(t)
  const capture = harness(source.config)
  const controller = new AbortController()
  controller.abort()
  assert.equal(await within(capture.invoke(controller.signal)), capture.accepted)
  assert.equal(capture.calls, 1)
  assert.match(capture.warnings[0], /cancelled/)
  assert.equal(existsSync(source.startedFile), false)
})

test('short-lived descendant output is drained after parent exit within the original deadline', async t => {
  const source = await fixture(t, { mode: 'finite' })
  const capture = harness({ ...source.config, timeoutMs: 3000 })
  const actual = await within(capture.invoke())
  const state = await source.exitedParent()
  assert.equal(actual.kind, 'accept')
  assert.deepEqual(JSON.parse(actual.content[0].text), envelope)
  assert.deepEqual(actual.additionalContexts, capture.accepted.additionalContexts)
  assert.equal(capture.calls, 1)
  assert.deepEqual(capture.warnings, [])
  await eventually(() => !running(state.descendant))
})

test('parent exit does not grant inherited pipes a fresh timeout', async t => {
  const source = await fixture(t, { parentDelay: 1200 })
  const capture = harness({ ...source.config, timeoutMs: 2000 })
  const started = Date.now()
  const pending = capture.invoke()
  const state = await source.exitedParent()
  assert.ok(existsSync(source.exitedFile), 'parent must exit on its own before the original deadline')
  assert.ok(running(state.descendant))
  assert.equal(await within(pending), capture.accepted)
  assert.ok(Date.now() - started < 2800, 'drainage must use the acquisition deadline')
  assert.match(capture.warnings[0], /timed out/)
  await eventually(() => !running(state.descendant))
})

for (const mode of ['hold', 'finite']) {
  test(`host exits naturally after ${mode === 'hold' ? 'stopping escaped capture' : 'successful capture'}`, async t => {
    const source = await fixture(t, { mode, escaped: mode === 'hold' })
    const moduleUrl = new URL('../index.js', import.meta.url).href
    const runner = `
      import { apply } from ${JSON.stringify(moduleUrl)}
      import { getEventListeners } from 'node:events'
      let listener
      const controller = new AbortController()
      const warnings = []
      const accepted = {kind:'accept'}
      apply({on: (_event, callback) => {listener = callback}, logger:{warn: m => warnings.push(m)}},
        ${JSON.stringify({ minBytes: 1, ...source.config, timeoutMs: mode === 'hold' ? 500 : 10000 })})
      const result = await listener({name:'fixture', signal:controller.signal},
        {content:[{type:'text', text:'x'.repeat(4096)}]}, async () => accepted)
      console.log(JSON.stringify({original:result === accepted, warnings, listeners:getEventListeners(controller.signal,'abort').length}))
    `
    // No process.exit() in the runner: leaked pipes, timers, or child handles
    // would keep it alive until this separate process guard terminates it.
    const run = spawnSync(process.execPath, ['--input-type=module', '-e', runner], {
      encoding: 'utf8', timeout: 4000,
    })
    assert.ifError(run.error)
    assert.equal(run.status, 0, run.stderr)
    const outcome = JSON.parse(run.stdout)
    assert.equal(outcome.original, mode === 'hold')
    assert.equal(outcome.listeners, 0)
    assert.equal(outcome.warnings.length, mode === 'hold' ? 1 : 0)
    if (mode === 'hold') {
      const state = await source.exitedParent()
      assert.ok(running(state.descendant), 'only the deliberately escaped fixture remains alive')
    }
  })
}
