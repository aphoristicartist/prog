// Test-only transport for a deterministic caller. No model, scheduler, or MCP
// server is involved. The plugin itself comes from the unpacked npm artifact.
import assert from 'node:assert/strict'
import { createInterface } from 'node:readline'
import { pathToFileURL } from 'node:url'
import { Context } from '@deepseek-ai/cordis'
import ToolRuntime from '@deepseek-ai/dsh-tools'
import SystemPrompt from '@deepseek-ai/dsh-system-prompt'
import LocalSubprocess from '@deepseek-ai/dsh-subprocess-local'
import SandboxPolicy from '@deepseek-ai/dsh-sandbox-policy'
import LocalSandbox from '@deepseek-ai/dsh-sandbox-local'
import { ShellEnvRegistry } from '@deepseek-ai/dsh-shell-env'

const [artifact, binary, project, store] = process.argv.slice(2)
const plugin = await import(pathToFileURL(artifact).href)
const ctx = new Context()
const fibers = []
const controller = new AbortController()
const stopped = () => controller.abort()
process.once('SIGTERM', stopped)
process.once('SIGINT', stopped)
try {
  for (const [service, options] of [
    [SystemPrompt, { includeHarnessIdentity: false }], [ToolRuntime, {}],
    [LocalSubprocess, {}], [SandboxPolicy, { mode: 'danger-full-access', workspaceRoot: project }],
    [LocalSandbox, {}], [ShellEnvRegistry, { dshHome: `${store}/host` }],
    [plugin, { progCommand: binary, storeDir: store, minBytes: 1, facade: true }],
  ]) fibers.push(await ctx.plugin(service, options))
  const assembly = await ctx.systemPrompt.assemble()
  assert.deepEqual(assembly.tools.map(tool => tool.name), ['prog_evidence', 'prog_observe', 'prog_status'])
  process.stdout.write(JSON.stringify({ assembly }) + '\n')
  let sequence = 0
  const lines = createInterface({ input: process.stdin })
  controller.signal.addEventListener('abort', () => lines.close(), { once: true })
  for await (const line of lines) {
    const request = JSON.parse(line)
    const result = await ctx.tools.execute({ ...request, callId: `installed-${++sequence}`, signal: controller.signal })
    process.stdout.write(JSON.stringify(result) + '\n')
  }
} finally {
  for (const fiber of fibers.reverse()) await fiber.dispose()
  process.removeListener('SIGTERM', stopped)
  process.removeListener('SIGINT', stopped)
}
