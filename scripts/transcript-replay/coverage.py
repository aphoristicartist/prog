"""Share of real test runs that prog's default shims capture and its providers judge,
replayed from local Claude Code and Codex transcripts (#312). Prints aggregates only."""
import sys, os, re, collections, subprocess, glob, importlib.util
spec = importlib.util.spec_from_file_location('sim', os.path.join(os.path.dirname(os.path.abspath(__file__)), 'gate_replay.py'))
sim = importlib.util.module_from_spec(spec); spec.loader.exec_module(sim)
TEST = re.compile(r"^(cargo (\+\S+ )?(test|nextest)|go test|(.*/)?(pytest|py\.test)|(.*/)?python3?(\.\d+)? -m (pytest|unittest)|(npm|pnpm|yarn|bun) (run )?(test|vitest|jest|e2e)|bun test|(npx|bunx|pnpx|pnpm exec|npm exec) (jest|vitest|playwright)|(.*/)?(jest|vitest)|playwright test|uv run (python -m )?(pytest|unittest)|deno test|swift test|xcodebuild test|lake (build|test)|make test|ctest)")
for label, files, parse in (
    ('claude', subprocess.run(['find', os.path.expanduser('~/.claude/projects'), '-name', '*.jsonl', '-mtime', '-120', '-not', '-path', '*/subagents/*'], capture_output=True, text=True).stdout.split(), lambda f: sim.claude_events(f)),
    ('codex', sorted(glob.glob(os.path.expanduser('~/.codex/sessions/**/*.jsonl'), recursive=True)), lambda f: sim.codex_events(f)[:2])):
    c = collections.Counter(); status = collections.Counter()
    for f in files:
        cwd, ev = parse(f)
        for e in ev:
            if e[0] != 'BASH': continue
            po = bool(re.search(r'(^|[\s;&|])(export\s+)?PATH=', e[1]))
            for argv in sim.simple_commands(e[1]):
                a = sim.normalize_argv(argv)
                if not a: continue
                s = ' '.join(a)
                if not TEST.match(s): continue
                prog = os.path.basename(a[0]); args = a[1:]
                captured = not po and '/' not in a[0] and prog in sim.SHIMS and sim.progressive(prog, args)
                kind = sim.provider(prog, args) if captured else None
                if captured and kind is None and prog in ('npm','pnpm','yarn') and args and (args[0] in ('test','t') or args[:2]==['run','test']) and sim.JS_SUMMARY.search(e[2] or ''): kind = 'js_text'
                if po: c[('PATH-prefixed', prog)] += 0
                st = 'judged' if kind else ('captured-unjudged' if captured else 'not-captured')
                route = re.sub(r'^(\S*/)', '', ' '.join([os.path.basename(a[0])] + a[1:3]))
                route = re.sub(r'\s+(-\S+|\S*[/.:]\S*|\S+::\S+).*$', '', route)
                c[(st, ' '.join(route.split()[:3]))] += 1; status[st] += 1
    tot = sum(status.values())
    print('=====', label, 'test runs', tot, {k: '%d (%.0f%%)' % (v, 100 * v / tot) for k, v in status.items()})
    for (st, r), v in c.most_common(22): print('   %-18s %-34s %d' % (st, r, v))
