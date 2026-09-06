import { resolve } from 'node:path'
import { defineTool } from '@deepseek-ai/dsh-tools'
import { invokeProg } from './facade-process.js'

export const name = 'prog-facade'
export const inject = ['tools', 'systemPrompt', 'subprocess', 'sandboxPolicy', 'shellEnv']
export const toolNames = Object.freeze(['prog_observe', 'prog_evidence', 'prog_status'])

export const instructions = 'Use prog_observe to capture exactly the requested argv, source call, file, or text. Use prog_evidence to inspect findings and retrieve cached evidence; omitted content is not absence. Use prog_status for the shared verification and delta decisions. A failed tool call never authorizes an automatic source retry. Required obligations must be declared by the user through the existing CLI. Advanced CLI navigation remains available for recovery. TTY and streaming execution are unsupported.'

function invalid(message) { throw new Error(`prog facade: ${message}`) }
function text(value, label, empty = false) {
  if (typeof value !== 'string' || (!empty && !value.length) || value.includes('\0') || !value.isWellFormed()) invalid(`invalid ${label}`)
  return value
}
function integer(value, fallback, min, max, label) {
  const result = value ?? fallback
  if (!Number.isSafeInteger(result) || result < min || result > max) invalid(`invalid ${label}`)
  return result
}
function keys(value, allowed) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) invalid('expected an argument object')
  if (Object.keys(value).some(key => !allowed.includes(key))) invalid('unsupported argument; TTY and streaming are unsupported')
}
function option(args, name, value) {
  if (value !== undefined) args.push(`--${name}=${text(value, name, true)}`)
}
function limit(value, fallback = 10) { return integer(value, fallback, 1, 100, 'limit') }

function configuration(input) {
  keys(input, ['progCommand', 'storeDir', 'budgetBytes', 'timeoutMs', 'graceMs', 'maxInputBytes'])
  return Object.freeze({
    progCommand: text(input.progCommand ?? 'prog', 'progCommand'),
    storeDir: input.storeDir === undefined ? undefined : text(input.storeDir, 'storeDir'),
    budgetBytes: integer(input.budgetBytes, 16384, 4096, 262144, 'budgetBytes'),
    timeoutMs: integer(input.timeoutMs, 30000, 1, 3600000, 'timeoutMs'),
    graceMs: integer(input.graceMs, 5000, 1000, 30000, 'graceMs'),
    maxInputBytes: integer(input.maxInputBytes, 16777216, 1, 67108864, 'maxInputBytes'),
  })
}

function observeRequest(args, config) {
  const common = ['mode', 'workdir', 'lens', 'comparison_family']
  const fields = {
    run: ['argv', 'timeout_ms'], file: ['file'], text: ['text', 'mime', 'name'],
    call: ['source', 'operation', 'arguments', 'yes', 'refresh'],
  }
  if (!Object.hasOwn(fields, args.mode)) invalid('invalid observe mode')
  keys(args, [...common, ...fields[args.mode]])
  let command
  let input
  let timeoutMs = config.timeoutMs
  if (args.mode === 'run') {
    if (!Array.isArray(args.argv) || args.argv.length === 0) invalid('argv must be a nonempty array')
    const argv = args.argv.map(value => text(value, 'argv element', true))
    text(argv[0], 'program')
    timeoutMs = integer(args.timeout_ms, config.timeoutMs, 1, config.timeoutMs, 'timeout_ms')
    command = ['run', `--timeout-ms=${timeoutMs}`]
    option(command, 'lens', args.lens)
    option(command, 'comparison-family', args.comparison_family)
    command.push('--', ...argv)
  } else if (args.mode === 'call') {
    if (args.arguments === undefined) invalid('source arguments are required')
    command = ['call', `--args=${JSON.stringify(args.arguments)}`]
    for (const flag of ['yes', 'refresh']) {
      if (args[flag] !== undefined && typeof args[flag] !== 'boolean') invalid(`invalid ${flag}`)
      if (args[flag]) command.push(`--${flag}`)
    }
    option(command, 'lens', args.lens)
    option(command, 'comparison-family', args.comparison_family)
    command.push('--', text(args.source, 'source'), text(args.operation, 'operation'))
  } else {
    command = ['observe', `--timeout-ms=${timeoutMs}`, `--max-input-bytes=${config.maxInputBytes}`]
    if (args.mode === 'file') command.push(`--file=${text(args.file, 'file')}`)
    else {
      input = text(args.text, 'text', true)
      if (Buffer.byteLength(input) > config.maxInputBytes) invalid('text exceeds the configured acquisition limit')
      command.push('--stdin')
      option(command, 'mime', args.mime)
      option(command, 'name', args.name)
    }
    option(command, 'lens', args.lens)
    option(command, 'comparison-family', args.comparison_family)
  }
  const workdir = args.workdir === undefined ? undefined : text(args.workdir, 'workdir')
  return {
    args: command, input, timeoutMs, schema: 'prog.disclosure',
    allowErrorDisclosure: args.mode === 'call',
    cwd: root => resolve(root, workdir ?? '.'),
    shellEnvironment: args.mode === 'run' || args.mode === 'call',
  }
}

function evidenceRequest(args) {
  const mode = args.mode ?? 'inspect'
  const fields = {
    inspect: ['cursor', 'path', 'goal', 'limit', 'kind'],
    exact: ['cursor', 'path', 'reference'],
    search: ['cursor', 'path', 'query', 'limit'],
    expand: ['cursor', 'path', 'limit', 'depth', 'out'],
  }
  if (!Object.hasOwn(fields, mode)) invalid('invalid evidence mode')
  keys(args, ['mode', ...fields[mode]])
  const reference = args.reference
  if (reference !== undefined) {
    if (args.cursor !== undefined || args.path !== undefined) invalid('use a reference or a cursor/path pair')
    for (const key of ['cursor', 'path', 'source_id', 'operation', 'redacted_slice_sha256']) text(reference?.[key], `reference ${key}`, key === 'path')
    if (!/^[0-9a-f]{64}$/.test(reference.redacted_slice_sha256)) invalid('invalid reference digest')
  }
  const cursor = text(reference?.cursor ?? args.cursor, 'cursor')
  const path = text(reference?.path ?? args.path ?? '', 'path', true)
  let command
  let schema
  if (mode === 'inspect') {
    command = ['inspect', `--goal=${text(args.goal ?? 'Find relevant failures', 'goal')}`, `--limit=${limit(args.limit)}`]
    option(command, 'kind', args.kind)
    schema = 'prog.inspect'
  } else if (mode === 'search') {
    command = ['search', `--limit=${limit(args.limit)}`]
    schema = 'prog.search'
  } else if (mode === 'expand') {
    command = ['expand', `--limit=${limit(args.limit)}`, `--depth=${integer(args.depth, 6, 1, 20, 'depth')}`]
    option(command, 'out', args.out)
    schema = 'prog.disclosure'
  } else {
    command = ['evidence']
    schema = 'prog.evidence'
  }
  command.push(`--path=${path}`, '--', cursor)
  if (mode === 'search') command.push(text(args.query, 'query'))
  return { args: command, schema, reference }
}

const string = description => ({ type: 'string', description })
const number = description => ({ type: 'integer', description })
const output = { schema: { type: 'json' }, render: (_args, value) => [{ type: 'text', text: JSON.stringify(value) }] }

export function apply(ctx, input = {}) {
  const config = configuration(input)
  const finalizeContent = (_exec, result) => {
    if (result.content.every(block => block.type === 'text')
      && result.content.reduce((bytes, block) => bytes + Buffer.byteLength(block.text), 0) <= config.budgetBytes) return undefined
    // Do not override a host denial or reinterpret a modified canonical value.
    // This is a presentation refusal, not a second disclosure engine.
    return [{ type: 'text', text: 'The host result presentation is unavailable because it exceeds the prog facade limit or uses unsupported content. Inspect host diagnostics; do not retry the source automatically.' }]
  }
  const invoke = (exec, request) => invokeProg(ctx, exec, config, {
    cwd: root => root, timeoutMs: config.timeoutMs,
    store: root => resolve(root, config.storeDir ?? '.prog'), ...request,
  })
  ctx.systemPrompt.section({ name: 'tools:prog-facade', order: 115, text: instructions })
  ctx.tools.register(defineTool({
    name: toolNames[0], description: 'Capture one exact command, configured source call, file, or text through prog under the host policy. Returns the canonical bounded observation and cursor, including captured source failures. No source retries.',
    parameters: {
      mode: { type: 'string', required: true, enum: ['run', 'call', 'file', 'text'] },
      argv: { type: 'array', items: { type: 'string' }, description: 'run: exact program and arguments; no shell parsing or command substitution.' },
      timeout_ms: number('run: source deadline, at most the configured timeout.'),
      file: string('file: artifact path, relative to workdir when relative.'),
      text: string('text: complete artifact content.'), mime: string('text: MIME type.'), name: string('text: artifact name.'),
      source: string('call: existing source profile id.'), operation: string('call: operation name.'), arguments: { type: 'json', description: 'call: operation arguments.' },
      yes: { type: 'boolean', description: 'call: forward explicit prior user confirmation to the existing --yes gate. Never confirms source-profile trust.' },
      refresh: { type: 'boolean', description: 'call: explicitly bypass cache and obtain a fresh observation.' },
      workdir: string('Effective cwd; relative paths are anchored to the host session workspace.'),
      lens: string('Optional existing lens id.'), comparison_family: string('Explicit comparison family; does not prove equal scope.'),
    }, output, finalizeContent,
    execute: (args, exec) => invoke(exec, observeRequest(args, config)),
  }))
  ctx.tools.register(defineTool({
    name: toolNames[1], description: 'Navigate cached evidence without rerunning its source. Inspect findings, retrieve an exact reference/path excerpt, expand an omitted region, or perform bounded literal search. Omissions and completeness remain explicit.',
    parameters: {
      mode: { type: 'string', enum: ['inspect', 'exact', 'expand', 'search'] },
      cursor: string('Existing cursor for inspect, path retrieval, expansion, or search.'), path: string('JSON Pointer within the cursor scope; defaults to its root.'),
      reference: { type: 'json', description: 'exact: an existing evidence_ref, including its cursor, path and digest. Exclusive with cursor/path.' },
      goal: string('inspect: evidence goal.'), kind: string('inspect: finding kind.'), query: string('search: literal query.'),
      limit: number('inspect/search/expand: 1–100 retained items.'), depth: number('expand: 1–20 levels.'),
      out: string('expand: explicitly export the complete cached slice to this file and return the canonical receipt. Read the file with the host file tool; account for that fallback.'),
    }, output, finalizeContent,
    async execute(args, exec) {
      const request = evidenceRequest(args)
      const value = await invoke(exec, request)
      if (request.reference && ['cursor', 'path', 'source_id', 'operation', 'redacted_slice_sha256']
        .some(key => value.evidence_ref?.[key] !== request.reference[key])) invalid('cached evidence does not match the supplied reference')
      return value
    },
  }))
  ctx.tools.register(defineTool({
    name: toolNames[2], description: 'Return canonical readiness, evidence availability and verification blockers. Optionally compare baseline/subject observations using the shared delta engine. Does not declare obligations or rerun sources.',
    parameters: { baseline: string('Earlier observation id; requires subject.'), subject: string('Later observation id; requires baseline.'), session_id: string('Existing prog session id; defaults to the active session.') }, output, finalizeContent,
    execute(args, exec) {
      keys(args, ['baseline', 'subject', 'session_id'])
      if ((args.baseline === undefined) !== (args.subject === undefined)) invalid('baseline and subject are required together')
      const command = ['status']
      for (const key of ['baseline', 'subject', 'session_id']) option(command, key.replaceAll('_', '-'), args[key])
      return invoke(exec, { args: command, schema: 'prog.status' })
    },
  }))
}
