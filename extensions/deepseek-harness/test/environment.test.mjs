import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'

const plugin = new URL('../index.js', import.meta.url).href

for (const fails of [false, true]) {
  test(`capture ${fails ? 'fallback' : 'success'} uses the host child environment policy`, async t => {
    const dir = await mkdtemp(join(tmpdir(), "prog-dsh-env 'fixture-"))
    t.after(() => rm(dir, { recursive: true, force: true }))
    const receipt = join(dir, 'environment.json')
    const helper = join(dir, 'capture.mjs')
    const runner = join(dir, 'host.mjs')
    // Synthetic credentials only. The fixture neither forwards nor prints
    // credentials from the developer/CI environment.
    const environment = {
      PATH: '/usr/bin:/bin',
      HOME: dir,
      TMPDIR: tmpdir(),
      LANG: 'C',
      HTTPS_PROXY: 'http://127.0.0.1:9',
      PROG_DIR: join(dir, 'store'),
      PROG_ENV_FIXTURE: 'ordinary child value',
      DEEPSEEK_API_KEY: 'synthetic-provider-credential',
      PROG_FIXTURE_PASSWORD: 'synthetic-password',
      PROG_FIXTURE_SECRET: 'synthetic-secret',
      prog_fixture_toKen: 'synthetic-token',
      DSH_WORKSPACE: 'stale-host-identity',
      dSh_fixture: 'stale-mixed-case-identity',
    }
    await writeFile(helper, `
      import { writeFileSync } from 'node:fs'
      for await (const chunk of process.stdin) { /* consume capture input */ }
      const names = ${JSON.stringify(Object.keys(environment))}
      const environment = Object.fromEntries(names
        .filter(name => Object.hasOwn(process.env, name))
        .map(name => [name, process.env[name]]))
      writeFileSync(${JSON.stringify(receipt)}, JSON.stringify({ environment, cwd: process.cwd() }))
      if (${fails}) process.exit(1)
      process.stdout.write(JSON.stringify({
        schema: 'prog.disclosure', cursor: 'pc1_environment_fixture',
        disclosure_verdict: { result: 'bounded_win' },
      }))
    `)
    await writeFile(runner, `
      import assert from 'node:assert/strict'
      import { apply } from ${JSON.stringify(plugin)}
      const before = { ...process.env }
      let listener
      const warnings = []
      let calls = 0
      apply({
        on: (_event, callback) => { listener = callback },
        logger: { warn: message => warnings.push(message) },
      }, {
        minBytes: 1, budgetBytes: 4096, timeoutMs: 2000,
        progCommand: process.execPath, progArgs: [${JSON.stringify(helper)}],
        cwd: ${JSON.stringify(dir)},
      })
      const decision = { kind: 'accept', additionalContexts: [{ type: 'text', text: 'host context' }] }
      const actual = await listener(
        { name: 'fixture', signal: new AbortController().signal },
        { content: [{ type: 'text', text: 'source result\\n'.repeat(300) }], isError: false },
        async () => { calls += 1; return decision },
      )
      assert.equal(calls, 1)
      assert.deepEqual({ ...process.env }, before, 'capture must not scrub the host process itself')
      if (${fails}) {
        assert.equal(actual, decision)
        assert.equal(warnings.length, 1)
      } else {
        assert.equal(JSON.parse(actual.content[0].text).cursor, 'pc1_environment_fixture')
        assert.deepEqual(actual.additionalContexts, decision.additionalContexts)
        assert.deepEqual(warnings, [])
      }
    `)
    const child = spawnSync(process.execPath, [runner], {
      env: environment, encoding: 'utf8', timeout: 5000,
    })
    assert.ifError(child.error)
    assert.equal(child.status, 0, child.stderr)
    const observed = JSON.parse(await readFile(receipt, 'utf8'))
    assert.deepEqual(observed, {
      cwd: await realpath(dir),
      environment: Object.fromEntries([
        'PATH', 'HOME', 'TMPDIR', 'LANG', 'HTTPS_PROXY', 'PROG_DIR', 'PROG_ENV_FIXTURE',
      ].map(name => [name, environment[name]])),
    })
  })
}
