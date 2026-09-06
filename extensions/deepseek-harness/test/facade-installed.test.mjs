import assert from 'node:assert/strict'
import { mkdtemp, readFile, rm, symlink } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawnSync } from 'node:child_process'
import test from 'node:test'

const binary = process.env.PROG_TEST_BINARY
const extension = fileURLToPath(new URL('..', import.meta.url))

test('unpacked npm artifact completes the installed coding loop through the real registry', { skip: !binary, timeout: 180000 }, async t => {
  const root = await mkdtemp(join(tmpdir(), "prog-installed-facade '"))
  t.after(() => rm(root, { recursive: true, force: true }))
  const run = (command, args, timeout = 10000) => {
    const result = spawnSync(command, args, { cwd: root, encoding: 'utf8', timeout, maxBuffer: 16 * 1024 * 1024 })
    assert.ifError(result.error)
    assert.equal(result.status, 0, result.stdout + result.stderr)
    return result.stdout
  }
  const packed = JSON.parse(run('npm', ['pack', extension, '--ignore-scripts', '--json']))[0]
  run('tar', ['-xzf', join(root, packed.filename), '-C', root])
  // Load the published file set outside checkout with the exact locked host
  // providers. This is not a network installer or a full dsh CLI profile test.
  await symlink(join(extension, 'node_modules'), join(root, 'node_modules'))
  const reportPath = join(root, 'report.json')
  run('python3', [resolve(extension, '../../fixtures/harness/registered_coding_loop.py'),
    '--prog', binary, '--node', process.execPath,
    '--driver', join(extension, 'fixtures/registered-loop-driver.mjs'),
    '--artifact', join(root, 'package/index.js'), '--output', reportPath], 160000)
  const report = JSON.parse(await readFile(reportPath, 'utf8'))
  assert.equal(report.passed, true)
  for (const arm of [report.cli, report.registered]) {
    assert.equal(arm.verified_status.readiness.ready, true)
    assert.equal(arm.test_executions.length, 7)
    for (const control of ['narrow', 'stale', 'incomplete', 'evicted']) assert.equal(arm.negative_controls[control].readiness.ready, false)
  }
  assert.ok(report.registered.deliveries.some(row => row.request.name === 'prog_observe'))
  assert.ok(report.registered.deliveries.some(row => row.request.name === 'prog_evidence'))
  assert.ok(report.registered.deliveries.some(row => row.request.name === 'prog_status'))
  for (const bytes of Object.values(report.registered.context_bytes)) assert.ok(Number.isSafeInteger(bytes) && bytes >= 0)
  assert.ok(report.registered.context_bytes.assembled_sections + report.registered.context_bytes.assembled_tool_schemas_json <= 8192,
    'review the fixed registered schema/instruction surface before raising its 8 KiB ceiling')
})
