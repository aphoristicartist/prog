import assert from 'node:assert/strict'
import { mkdtemp, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { spawnSync } from 'node:child_process'
import test from 'node:test'

import { apply, testing } from '../index.js'

function context() {
  let listener
  const warnings = []
  return {
    ctx: {
      logger: { warn: message => warnings.push(message) },
      on(name, candidate) {
        assert.equal(name, 'tools/post-execute')
        listener = candidate
      },
    },
    warnings,
    invoke: (...args) => listener(...args),
  }
}

async function mockProg(envelope) {
  const dir = await mkdtemp(join(tmpdir(), 'prog-dsh-test-'))
  const script = join(dir, 'mock-prog.mjs')
  await writeFile(script, `
    import process from 'node:process'
    let input = ''
    for await (const chunk of process.stdin) input += chunk
    if (!process.argv.includes('observe') || !process.argv.includes('--stdin')) process.exit(64)
    process.stdout.write(JSON.stringify(${JSON.stringify(envelope)}))
  `)
  return script
}

async function slowProg() {
  const dir = await mkdtemp(join(tmpdir(), 'prog-dsh-slow-test-'))
  const script = join(dir, 'slow-prog.mjs')
  await writeFile(script, 'setTimeout(() => {}, 10_000)\n')
  return script
}

function result(text) {
  return { content: [{ type: 'text', text }], isError: false }
}

test('plain-text detection is conservative', () => {
  assert.equal(testing.plainText([{ type: 'text', text: 'a' }, { type: 'text', text: 'b' }]), 'ab')
  assert.equal(testing.plainText([{ type: 'image', data: 'x' }]), undefined)
  assert.equal(testing.looksLikeProgResult('{"schema":"prog.disclosure"}'), true)
  assert.equal(testing.looksLikeProgResult('ordinary output'), false)
})

test('small results pass through without invoking prog', async () => {
  const harness = context()
  apply(harness.ctx, { minBytes: 32, progCommand: '/definitely/missing/prog' })
  const accepted = { kind: 'accept' }
  const actual = await harness.invoke({ name: 'bash' }, result('small'), async () => accepted)
  assert.equal(actual, accepted)
  assert.deepEqual(harness.warnings, [])
})

test('unsupported, nested, blocked, and typed results pass through', async () => {
  const cases = [
    {
      exec: { name: 'image' },
      source: { content: [{ type: 'image', data: 'x' }], isError: false },
      decision: { kind: 'accept' },
    },
    {
      exec: { name: 'bash', parent: { callId: 'parent' } },
      source: result('x'.repeat(1024)),
      decision: { kind: 'accept' },
    },
    {
      exec: { name: 'bash' },
      source: result('x'.repeat(1024)),
      decision: { kind: 'accept', value: { typed: true } },
    },
    {
      exec: { name: 'bash' },
      source: result('x'.repeat(1024)),
      decision: { kind: 'block', feedback: [{ type: 'text', text: 'denied' }] },
    },
    {
      exec: { name: 'bash' },
      source: result('{"schema":"prog.disclosure","cursor":"pc1_existing"}'),
      decision: { kind: 'accept' },
    },
  ]
  for (const fixture of cases) {
    const harness = context()
    apply(harness.ctx, { minBytes: 10, progCommand: '/definitely/missing/prog' })
    const actual = await harness.invoke(fixture.exec, fixture.source, async () => fixture.decision)
    assert.equal(actual, fixture.decision)
    assert.deepEqual(harness.warnings, [])
  }
})

test('a bounded disclosure replaces the original result without rerunning the tool', async () => {
  const envelope = {
    schema: 'prog.disclosure',
    cursor: 'pc1_fixture',
    disclosure_verdict: { result: 'bounded_win' },
    observation: { safety: { redacted_before_persistence: false } },
  }
  const script = await mockProg(envelope)
  const harness = context()
  apply(harness.ctx, {
    minBytes: 10,
    budgetBytes: 4096,
    progCommand: process.execPath,
    progArgs: [script],
  })
  const original = 'x'.repeat(4096)
  const actual = await harness.invoke({ name: 'bash', callId: 'call-1' }, result(original), async () => ({ kind: 'accept' }))
  assert.equal(actual.kind, 'accept')
  assert.deepEqual(JSON.parse(actual.content[0].text), envelope)
  assert.deepEqual(harness.warnings, [])
})

test('raw-cheaper results and adapter failures fail open to the original result', async () => {
  const rawEnvelope = {
    schema: 'prog.disclosure',
    cursor: 'pc1_raw',
    disclosure_verdict: { result: 'raw_cheaper' },
    observation: { safety: { redacted_before_persistence: false } },
  }
  const script = await mockProg(rawEnvelope)
  const invalidScript = await mockProg({ schema: 'unexpected.result' })
  const incompleteScript = await mockProg({ schema: 'prog.disclosure', cursor: 'pc1_incomplete' })
  for (const config of [
    { progCommand: process.execPath, progArgs: [script] },
    { progCommand: process.execPath, progArgs: [invalidScript] },
    { progCommand: process.execPath, progArgs: [incompleteScript] },
    { progCommand: '/definitely/missing/prog' },
  ]) {
    const harness = context()
    apply(harness.ctx, { minBytes: 10, budgetBytes: 4096, ...config })
    const accepted = { kind: 'accept' }
    const actual = await harness.invoke({ name: 'bash' }, result('x'.repeat(1024)), async () => accepted)
    assert.equal(actual, accepted)
  }
})

test('redaction forces replacement even when raw output would be cheaper', async () => {
  const envelope = {
    schema: 'prog.disclosure',
    cursor: 'pc1_redacted',
    disclosure_verdict: { result: 'raw_cheaper' },
    observation: { safety: { redacted_before_persistence: true } },
  }
  const script = await mockProg(envelope)
  const harness = context()
  apply(harness.ctx, {
    minBytes: 10,
    budgetBytes: 4096,
    progCommand: process.execPath,
    progArgs: [script],
  })
  const actual = await harness.invoke({ name: 'bash' }, result('secret'.repeat(1024)), async () => ({ kind: 'accept' }))
  assert.equal(actual.kind, 'accept')
  assert.deepEqual(JSON.parse(actual.content[0].text), envelope)
})

test('timeout and harness cancellation preserve the original successful result', async () => {
  const script = await slowProg()
  for (const cancel of [false, true]) {
    const harness = context()
    apply(harness.ctx, {
      minBytes: 10,
      timeoutMs: cancel ? 10_000 : 20,
      progCommand: process.execPath,
      progArgs: [script],
    })
    const accepted = { kind: 'accept' }
    const controller = new AbortController()
    if (cancel) setTimeout(() => controller.abort(), 20)
    const started = Date.now()
    const actual = await harness.invoke(
      { name: 'bash', signal: controller.signal },
      result('x'.repeat(1024)),
      async () => accepted,
    )
    assert.equal(actual, accepted)
    assert.equal(harness.warnings.length, 1)
    assert.ok(Date.now() - started < 2_000)
  }
})

test('the native adapter captures and retrieves evidence through the real prog binary', {
  skip: process.env.PROG_TEST_BINARY === undefined,
}, async () => {
  const binary = resolve(process.env.PROG_TEST_BINARY)
  const storeDir = await mkdtemp(join(tmpdir(), 'prog-dsh-e2e-'))
  const harness = context()
  apply(harness.ctx, {
    minBytes: 10,
    budgetBytes: 4096,
    progCommand: binary,
    storeDir,
  })
  const original = 'exact-evidence-line\n'.repeat(2_000)
  const actual = await harness.invoke(
    { name: 'bash', callId: 'call-e2e' },
    result(original),
    async () => ({ kind: 'accept' }),
  )
  const envelope = JSON.parse(actual.content[0].text)
  assert.equal(envelope.schema, 'prog.disclosure')
  assert.match(envelope.cursor, /^pc1_/)
  assert.equal(envelope.disclosure_verdict.result, 'bounded_win')

  const evidence = spawnSync(binary, [
    '--dir', storeDir, 'evidence', envelope.cursor, '--path', '/lines/0/text',
  ], { encoding: 'utf8' })
  assert.equal(evidence.status, 0, evidence.stderr)
  const block = JSON.parse(evidence.stdout)
  assert.equal(block.schema, 'prog.evidence')
  assert.equal(block.path, '/lines/0/text')
  assert.match(block.excerpt, /exact-evidence-line/)
})
