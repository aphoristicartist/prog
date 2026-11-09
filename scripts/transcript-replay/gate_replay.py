"""Replay prog's stop gate (origin/main 18c8c15, hook.rs:155-247) over real
Claude Code and Codex transcripts, plus the #305 claim-contradiction rule.

Modelled from the code:
- only commands the default PATH shims capture count (shim.rs:28 DEFAULT_SHIM_TOOLS,
  captured only when route.rs classify_route says Progressive);
- a captured run is a "head" candidate only if a coding provider parses it
  (coding_provider.rs provider_kind: cargo test/build/check/clippy/bench, pytest,
  python -m pytest|unittest [python is NOT shimmed], go test, jest/vitest);
- chains are keyed by the exact invocation (store.rs:400 invocation_fingerprint):
  argv + cwd;
- at every Stop: block if any head is Failing or NotProven; if no head: block when
  the workspace changed since session start unless an unjudged verification run was
  captured; if all heads green: block when files changed after the newest head.
Unknowable from transcripts (reported as variants): whether HEAD moves (git commit)
count as workspace changes; when a fail->pass is NotProven.
"""
import json, os, re, sys, glob, shlex, subprocess, collections, functools

SHIMS = {"cargo", "pytest", "py.test", "go", "npm", "pnpm", "yarn", "bun", "deno", "jest",
         "vitest", "playwright", "tsc", "ruff", "biome", "uv"}


def progressive(prog, args):
    first = args[0] if args else None
    if prog in ("pytest", "py.test", "jest"):
        return True
    if prog == "tsc":
        return not any(a in ("-v", "--version", "-h", "--help") for a in args)
    if prog == "cargo":
        a = args[1] if first and first.startswith("+") and len(args) > 1 else first
        return a in ("test", "check", "clippy", "build", "bench", "run")
    if prog == "go":
        return first == "test"
    if prog == "deno":
        return first in ("test", "lint", "check")
    if prog in ("npm", "pnpm", "yarn", "bun"):
        return first in ("test", "run", "audit")
    if prog == "vitest":
        return first == "run"
    if prog == "playwright":
        return first == "test"
    if prog == "uv":
        return first == "run"
    if prog == "ruff":
        return first == "check"
    if prog == "biome":
        return first in ("check", "ci", "lint")
    return False


def provider(prog, args):
    """Which coding provider would parse this captured argv (None = unjudged)."""
    first = args[0] if args else None
    if prog in ("pytest", "py.test"):
        return "pytest"
    if prog == "go" and first == "test":
        return "go"
    if prog in ("jest", "vitest"):
        return "js"
    if prog in ("pnpm", "yarn") and first in ("jest", "vitest"):
        return "js"
    if prog in ("npm", "pnpm", "yarn", "bun") and first in ("exec", "x") and len(args) > 1 and args[1] in ("jest", "vitest"):
        return "js"
    if prog == "cargo":
        a = args[1] if first and first.startswith("+") and len(args) > 1 else first
        if a in ("test", "bench"):
            return "cargo_test"
        if a in ("build", "check", "clippy", "rustc"):
            return "cargo_build"
    return None


def is_verification(prog, args):
    first = args[0] if args else None
    if prog in ("pytest", "py.test", "jest", "vitest", "mocha", "rustc", "tsc", "mypy", "pyright", "eslint",
                "ruff", "biome", "cargo", "make", "ctest", "gradle", "mvn", "rspec", "phpunit"):
        return True
    if prog == "go":
        return first in ("test", "build", "vet")
    if prog == "deno":
        return first in ("test", "lint", "check")
    if prog in ("npm", "pnpm", "yarn", "bun"):
        return first in ("test", "run", "exec", "x")
    if prog in ("npx", "bunx", "uv", "uvx", "poetry", "python", "python3"):
        return any(os.path.basename(a) in ("pytest", "jest", "vitest", "mypy", "ruff", "eslint", "tsc", "unittest") for a in args)
    if prog == "playwright":
        return first == "test"
    return False


HEREDOC = re.compile(r"<<-?\s*['\"]?(\w+)['\"]?")
WRAPPERS = {"env", "time", "nice", "command", "exec", "nohup", "caffeinate", "stdbuf"}


def strip_heredocs(cmd):
    out, lines, i = [], cmd.split("\n"), 0
    while i < len(lines):
        line = lines[i]
        out.append(line)
        m = HEREDOC.search(line)
        if m:
            tag = m.group(1)
            i += 1
            while i < len(lines) and lines[i].strip() != tag:
                i += 1
        i += 1
    return "\n".join(out)


def simple_commands(cmd):
    """Yield argv lists of the simple commands in a shell string (best effort)."""
    cmd = strip_heredocs(cmd)
    cmd = re.sub(r"\d*>&\d+|&>>?\s*\S+|\d*>>?\s*[^\s;&|]+|<\s*[^\s;&|<]+", " ", cmd)
    for line in cmd.split("\n"):
        try:
            lex = shlex.shlex(line, posix=True, punctuation_chars=";&|<>()")
            lex.whitespace_split = True
            toks = list(lex)
        except ValueError:
            toks = line.split()
        cur = []
        for t in toks + [";"]:
            if t and set(t) <= set(";&|()"):
                if cur:
                    yield cur
                cur = []
            elif t and set(t) <= set("<>"):
                cur.append("\x00redir")
            else:
                cur.append(t)


def normalize_argv(argv):
    a = [x for x in argv]
    # drop redirect targets
    out, skip = [], False
    for x in a:
        if skip:
            skip = False
            continue
        if x == "\x00redir":
            skip = True
            continue
        if re.fullmatch(r"\d?>&?\d?", x):
            continue
        out.append(x)
    a = out
    while a and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*=.*", a[0]):
        a = a[1:]
    while a and os.path.basename(a[0]) in WRAPPERS | {"timeout", "gtimeout"}:
        w = os.path.basename(a[0])
        a = a[1:]
        if w in ("timeout", "gtimeout"):
            while a and a[0].startswith("-"):
                a = a[1:]
            if a:
                a = a[1:]
        while a and (a[0].startswith("-") or re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*=.*", a[0])):
            a = a[1:]
    return a


JS_SUMMARY = re.compile(r"^\s*Tests:\s+.*\d+ (passed|failed)|^\s*Test Files\s+.*\d+ (passed|failed)", re.M)
FAIL_MARK = {
    "js_text": re.compile(r"^\s*Tests:.*\d+ failed|^\s*Test Files.*\d+ failed|^\s*Tests\s+.*\d+ failed", re.M),
    "cargo_test": re.compile(r"test result: FAILED|error: test failed|could not compile|^error(\[E\d+\])?:|error: \d+ targets? failed", re.M),
    "cargo_build": re.compile(r"could not compile|^error(\[E\d+\])?:|^error: ", re.M),
    "pytest": re.compile(r"\b\d+ (failed|errors?)\b|ERROR collecting|INTERNALERROR|^E\s+\w+Error", re.M),
    "go": re.compile(r"^(FAIL|--- FAIL)|\[build failed\]|\[setup failed\]", re.M),
    "js": re.compile(r"Tests:\s+\d+ failed|\b\d+ failed\b|FAIL\s", re.M),
}
PASS_MARK = {
    "js_text": re.compile(r"\d+ passed"),
    "cargo_test": re.compile(r"test result: ok"),
    "cargo_build": re.compile(r"Finished [`']?\w+"),
    "pytest": re.compile(r"\b\d+ passed\b"),
    "go": re.compile(r"^ok\s", re.M),
    "js": re.compile(r"Tests:\s+\d+ passed|\b\d+ passed\b"),
}


def run_ok(kind, out, tool_error):
    if kind in FAIL_MARK and FAIL_MARK[kind].search(out or ""):
        return False
    if kind in PASS_MARK and PASS_MARK[kind].search(out or ""):
        return True
    return not tool_error


def names_passing(kind, args, out):
    """Would the run name its passing tests (needed to prove a fix)?"""
    if kind == "pytest":
        return any(a in ("-v", "-vv", "--verbose", "-rA") or re.fullmatch(r"-[a-zA-Z]*v[a-zA-Z]*", a) for a in args)
    if kind == "cargo_test":
        return not any(a in ("-q", "--quiet") for a in args)
    if kind == "go":
        return "-v" in args or "-json" in args
    if kind == "js":
        return any("json" in a for a in args)
    return False


def rerun_has_skips(kind, out):
    if kind == "cargo_test":
        return bool(re.search(r"[1-9]\d* ignored|[1-9]\d* filtered out", out or ""))
    if kind == "pytest":
        return bool(re.search(r"\b\d+ (skipped|deselected|xfailed)\b", out or ""))
    if kind == "go":
        return "--- SKIP" in (out or "")
    return False


EDIT_BASH = re.compile(r"(^|[\s;&|])(sed\s+-i|perl\s+-[a-z]*i|git\s+(checkout|restore|apply|stash|reset|merge|rebase|cherry-pick|pull|am|switch)\b|cargo\s+(fmt|fix)\b|prettier\s+.*--write|ruff\s+format|ruff\s+check\s+.*--fix|black\s|(npm|pnpm|yarn)\s+(install|add|i)\b|patch\s+-p|tee\s+[^-/])")
HEAD_MOVE = re.compile(r"(^|[\s;&|])git\s+(commit|merge|rebase|pull|checkout|switch|cherry-pick|am|reset)\b")
REDIRECT_TO_REPO = re.compile(r"(?<![0-9&])>>?\s*(?!/dev/|/tmp/|/private/|\$TMPDIR|&)([\w.][\w./-]*\.(rs|py|ts|tsx|js|go|md|toml|json|ya?ml|sh|txt|css|html))\b")

CLAIM_RE = [
    re.compile(r"\ball\s+(the\s+)?(\d+\s+)?(unit\s+|integration\s+)?tests?\s+(now\s+)?pass(ed|ing|es)?\b", re.I),
    re.compile(r"\b(\d[\d,]*)\s+(tests?\s+)?passed(,\s*0\s+failed)?\b", re.I),
    re.compile(r"\b0\s+failed\b", re.I),
    re.compile(r"\b(tests?|suite|build|clippy|ci|gate|checks?)\s+(are\s+|is\s+)?(now\s+)?(all\s+)?(pass(es|ing)?|green|clean)\b", re.I),
    re.compile(r"\b(proven|verified)\s+fixed\b|\bnow\s+passes\b|\bis\s+fixed\b|\bare\s+fixed\b|\bfixed\s+and\s+(tested|verified)\b", re.I),
]
HEDGE = re.compile(r"\b(not|n't|never|no longer|still\s+fail|could\s+not|couldn't|unable|unverified|unproven|without\s+running|if\s|once\s|would|should|will\s+pass|until|whether|expect|unless|before\s+you|need\s+to|to\s+make|pending|haven't|didn't|hasn't|fail(s|ed|ing)?\s+(on|in|with))", re.I)
QUOTE_OR_CODE = re.compile(r"`[^`]*`|\"[^\"]*\"|“[^”]*”|^>.*$", re.M)


def claims_success(text):
    if not text:
        return None
    body = re.sub(r"```.*?```", " ", text, flags=re.S)
    body = QUOTE_OR_CODE.sub(" ", body)
    for sent in re.split(r"(?<=[.!?])\s+|\n+", body):
        for rx in CLAIM_RE:
            m = rx.search(sent)
            if m and not HEDGE.search(sent):
                return sent.strip()[:200]
    return None


CORRECTION = re.compile(r"\b(tests?|build|ci|clippy|lint)\b[^.\n]{0,40}\b(fail\w*|broken|red|error\w*)\b|did you (run|test)|still (fail\w*|broken|not working)|doesn'?t (work|compile|build)|you (didn'?t|did not|haven'?t) (run|test)", re.I)


@functools.lru_cache(maxsize=None)
def git_root(path):
    try:
        r = subprocess.run(["git", "-C", path, "rev-parse", "--show-toplevel"], capture_output=True, text=True, timeout=5)
        return r.stdout.strip() or None if r.returncode == 0 else None
    except Exception:
        return None


IGNORE_GUESS = re.compile(r"/(target|node_modules|\.venv|venv|dist|build|__pycache__|\.pytest_cache|\.git|\.next|\.turbo)/")


def in_workspace(path, root):
    if not root or not path:
        return False
    p = os.path.normpath(path if os.path.isabs(path) else os.path.join(root, path))
    return (p == root or p.startswith(root + "/")) and not IGNORE_GUESS.search(p + "/")


# ---------------------------------------------------------------- parsers
def claude_events(f):
    cwd = None
    ev = []
    pending = {}
    for line in open(f, errors="replace"):
        try:
            d = json.loads(line)
        except Exception:
            continue
        if d.get("isSidechain"):
            continue
        cwd = d.get("cwd") or cwd
        m = d.get("message") or {}
        if d.get("type") == "assistant":
            for b in m.get("content") or []:
                if not isinstance(b, dict):
                    continue
                if b.get("type") == "tool_use":
                    inp = b.get("input") or {}
                    name = b.get("name")
                    if name in ("Edit", "Write", "MultiEdit", "NotebookEdit"):
                        ev.append(("EDIT", inp.get("file_path") or inp.get("notebook_path"), "tool", cwd))
                    elif name == "Bash":
                        pending[b.get("id")] = (inp.get("command", ""), cwd)
                    ev.append(("TOOL", name))
                elif b.get("type") == "text" and b.get("text", "").strip():
                    ev.append(("TEXT", b["text"]))
        elif d.get("type") == "user" and not d.get("isMeta") and not d.get("isCompactSummary"):
            c = m.get("content")
            if isinstance(c, list):
                for x in c:
                    if isinstance(x, dict) and x.get("type") == "tool_result" and x.get("tool_use_id") in pending:
                        cmd, ccwd = pending.pop(x["tool_use_id"])
                        out = x.get("content")
                        if isinstance(out, list):
                            out = "\n".join(y.get("text", "") for y in out if isinstance(y, dict))
                        ev.append(("BASH", cmd, str(out or ""), bool(x.get("is_error")), ccwd))
                texts = [x.get("text", "") for x in c if isinstance(x, dict) and x.get("type") == "text"]
                if texts and not any(t.startswith(("[Request interrupted", "<local-command", "<command-name>", "Caveat:")) for t in texts):
                    ev.append(("USER", "\n".join(texts)))
            elif isinstance(c, str) and not c.startswith(("<local-command", "<command-name>", "Caveat:", "[Request interrupted")):
                ev.append(("USER", c))
    # turn ends: a TEXT with no later TOOL/TEXT before the next USER (or EOF)
    out = []
    for i, e in enumerate(ev):
        out.append(e)
        if e[0] == "TEXT":
            j = i + 1
            while j < len(ev) and ev[j][0] not in ("USER", "TEXT", "TOOL", "BASH", "EDIT"):
                j += 1
            if j >= len(ev) or ev[j][0] == "USER":
                out.append(("STOP", e[1]))
    return cwd, out


def codex_events(f):
    ev = []
    cwd = None
    top = True
    for line in open(f, errors="replace"):
        try:
            o = json.loads(line)
        except Exception:
            continue
        p = o.get("payload") or {}
        t = o.get("type")
        if t == "session_meta":
            cwd = p.get("cwd")
            src = p.get("source")
            top = isinstance(src, str)  # sub-agent threads carry an object source
        if t != "event_msg":
            continue
        pt = p.get("type")
        if pt == "item_completed":
            it = p.get("item") or {}
            ty = it.get("type")
            if ty == "UserMessage":
                ev.append(("USER", "\n".join(x.get("text", "") for x in it.get("content") or [] if isinstance(x, dict))))
            elif ty == "CommandExecution":
                cmd = it.get("command") or []
                s = cmd[2] if len(cmd) >= 3 and cmd[1] in ("-lc", "-c") else " ".join(map(str, cmd))
                c = (it.get("cwd") or "").replace("file://", "") or cwd
                ev.append(("TOOL", "exec"))
                ev.append(("BASH", s, it.get("aggregated_output") or "", (it.get("exit_code") not in (0, None)), c))
            elif ty == "FileChange":
                for path in (it.get("changes") or {}):
                    ev.append(("EDIT", path, "tool", cwd))
                ev.append(("TOOL", "apply_patch"))
            elif ty == "AgentMessage":
                txt = "\n".join(x.get("text", "") for x in it.get("content") or [] if isinstance(x, dict))
                if txt.strip():
                    ev.append(("TEXT", txt))
        elif pt == "task_complete":
            if p.get("error"):
                continue
            ev.append(("STOP", p.get("last_agent_message") or ""))
    return cwd, ev, top


# ---------------------------------------------------------------- simulator
def simulate(cwd, ev, stats, rows, label, head_moves_count=False, notproven_mode="optimistic"):
    root = git_root(cwd) if cwd and os.path.isdir(cwd) else None
    git = root is not None
    root = root or cwd
    chains = {}          # key -> list of runs
    captured_seq = []    # seq of every captured observation (judged or not)
    seq = 0
    ws_changed_since_start = False
    last_edit_seq = -1
    unjudged = False
    turn_has_edit = False
    last_any_verif_seq = -1
    last_code_edit_seq = -1
    for e in ev:
        if e[0] == "EDIT":
            path = e[1]
            if in_workspace(path, root):
                seq += 1
                ws_changed_since_start = True
                last_edit_seq = seq
                if not re.search(r"\.(md|mdx|txt|rst|adoc|csv|html?|svg|png|jpe?g|lock|log)$|/docs?/|LICENSE|CHANGELOG", path or "", re.I):
                    last_code_edit_seq = seq
                stats[label, "edits_in_workspace"] += 1
                turn_has_edit = True
        elif e[0] == "BASH":
            cmd, out, err, ccwd = e[1], e[2], e[3], e[4]
            if EDIT_BASH.search(cmd) or REDIRECT_TO_REPO.search(cmd) or (head_moves_count and HEAD_MOVE.search(cmd)):
                if not re.search(r"scratchpad|/tmp/|/private/var", cmd):
                    seq += 1
                    ws_changed_since_start = True
                    last_edit_seq = seq
                    stats[label, "bash_edits"] += 1
                    turn_has_edit = True
            path_override = bool(re.search(r"(^|[\s;&|])(export\s+)?PATH=", cmd))
            for argv in simple_commands(cmd):
                a = normalize_argv(argv)
                if not a:
                    continue
                prog = os.path.basename(a[0])
                args = a[1:]
                # an explicit path, or a PATH the command prepends itself, bypasses PATH shims
                by_path = "/" in a[0] or path_override
                if is_verification(prog, args) or (by_path and prog in ("pytest", "py.test", "jest", "vitest")):
                    seq += 1
                    last_any_verif_seq = seq
                if by_path or prog not in SHIMS or not progressive(prog, args):
                    if is_verification(prog, args):
                        stats[label, "verification_not_captured"] += 1
                        stats[label, "nc:" + prog] += 1
                    continue
                kind = provider(prog, args)
                if kind is None and prog in ("npm", "pnpm", "yarn") and args and (args[0] in ("test", "t", "tst") or args[:2] == ["run", "test"]) \
                        and JS_SUMMARY.search(out or ""):
                    kind = "js_text"
                stats[label, "captured_runs"] += 1
                seq += 1
                captured_seq.append(seq)
                if kind is None:
                    if is_verification(prog, args):
                        unjudged = True
                        stats[label, "captured_unjudged"] += 1
                        stats[label, "uj:" + prog + " " + (args[0] if args else "")] += 1
                    continue
                seq += 1
                ok = run_ok(kind, out, err)
                key = (ccwd, tuple(a))
                prev = chains.get(key, [])
                verdict = "pass" if ok else "fail"
                if ok and prev and prev[-1]["verdict"] in ("fail", "notproven"):
                    proven = names_passing(kind, args, out) and not rerun_has_skips(kind, out)
                    if notproven_mode == "optimistic":
                        verdict = "fixed"
                    else:
                        verdict = "fixed" if proven else "notproven"
                    stats[label, "fail_to_pass"] += 1
                    stats[label, "fail_to_pass_unprovable_guess"] += (not proven)
                chains.setdefault(key, []).append({"verdict": verdict, "seq": seq, "kind": kind, "argv": a})
                stats[label, "parsed_runs"] += 1
                stats[label, "parsed:" + kind] += 1
        elif e[0] == "STOP":
            stats[label, "turn_ends"] += 1
            if turn_has_edit:
                stats[label, "turn_ends_with_edit_in_turn"] += 1
            window = captured_seq[-100]  if len(captured_seq) >= 100 else -1
            heads = [runs[-1] for runs in chains.values() if runs[-1]["seq"] >= window]
            reason = None
            bad = [h for h in heads if h["verdict"] in ("fail", "notproven")]
            if bad:
                reason = "failing" if any(h["verdict"] == "fail" for h in bad) else "notproven"
                # was the failing head superseded in spirit by a later passing run of a different invocation?
                newest_any = max(heads, key=lambda h: h["seq"])
                if newest_any["verdict"] in ("pass", "fixed") and all(h["seq"] < newest_any["seq"] for h in bad):
                    stats[label, "block_failing_head_but_newer_run_passed"] += 1
            elif not heads:
                if not unjudged and git and ws_changed_since_start:
                    reason = "no_test"
            else:
                newest = max(heads, key=lambda h: h["seq"])
                if git and last_edit_seq > newest["seq"]:
                    reason = "stale"
            claim = claims_success(e[1])
            if claim:
                stats[label, "turn_ends_claiming_success"] += 1
            if reason == "no_test":
                if last_any_verif_seq > -1:
                    stats[label, "no_test_but_uncaptured_verification_ran"] += 1
                if last_any_verif_seq > last_edit_seq:
                    stats[label, "no_test_but_uncaptured_verification_after_last_edit"] += 1
            if reason:
                stats[label, "would_block"] += 1
                stats[label, "block:" + reason] += 1
                if claim:
                    stats[label, "block_and_claim"] += 1
            # #305: contradiction = claim + (failing/notproven head, or stale after last passing run); never on missing evidence
            contra = None
            if claim and heads:
                if bad:
                    contra = reason
                else:
                    newest = max(heads, key=lambda h: h["seq"])
                    if git and last_edit_seq > newest["seq"]:
                        contra = "stale"
            # variant: judge only the newest parsed run (older chains never refute)
            newest_rule = None
            if heads:
                nh = max(heads, key=lambda h: h["seq"])
                if nh["verdict"] in ("fail", "notproven"):
                    newest_rule = nh["verdict"]
                elif git and last_edit_seq > nh["seq"]:
                    newest_rule = "stale"
            elif not unjudged and git and ws_changed_since_start:
                newest_rule = "no_test"
            proposed = None
            if heads:
                nh = max(heads, key=lambda h: h["seq"])
                if nh["verdict"] == "fail":
                    proposed = "newest_failing"
                elif git and last_code_edit_seq > nh["seq"]:
                    proposed = "code_changed_after_newest_run"
            if proposed:
                stats[label, "proposed_blocks"] += 1
                stats[label, "proposed:" + proposed] += 1
                if claim:
                    stats[label, "proposed_and_claim"] += 1
            if newest_rule:
                stats[label, "newest_gate_blocks"] += 1
                stats[label, "newest_gate:" + newest_rule] += 1
            ci_only = bool(claim) and bool(re.search(r"\b(CI|checks?|workflow|jobs?|github)\b", claim, re.I)) and not re.search(r"\blocal(ly)?\b|\bcargo\b|\bpytest\b", claim, re.I)
            c305 = None
            if claim and not ci_only and heads:
                nh = max(heads, key=lambda h: h["seq"])
                if nh["verdict"] in ("fail", "notproven"):
                    c305 = nh["verdict"]
                elif git and last_edit_seq > nh["seq"]:
                    c305 = "stale"
            if c305:
                stats[label, "305newest_fires"] += 1
                stats[label, "305newest:" + c305] += 1
            broad_stale = bool(claim) and not ci_only and git and last_edit_seq > last_any_verif_seq
            if broad_stale:
                stats[label, "claims_stale_vs_any_verification"] += 1
            if claim and not ci_only:
                stats[label, "local_claims"] += 1
            if contra:
                stats[label, "305_fires"] += 1
                stats[label, "305:" + contra] += 1
            rows.append({"file": CURRENT[0], "label": label, "reason": reason, "claim": claim, "contra": contra,
                         "text": (e[1] or "")[-600:], "heads": [(" ".join(h["argv"])[:100], h["verdict"], h["seq"]) for h in heads],
                         "c305": c305, "proposed": proposed, "newest_rule": newest_rule, "broad_stale": broad_stale, "ci_only": ci_only, "edit_after_newest": bool(heads) and last_edit_seq > max(h["seq"] for h in heads)})
            turn_has_edit = False
        elif e[0] == "USER":
            if rows and rows[-1]["label"] == label and "next_user" not in rows[-1]:
                rows[-1]["next_user"] = e[1][:400]
                if CORRECTION.search(e[1] or ""):
                    rows[-1]["user_correction"] = True


CURRENT = [None]


def main():
    head_moves = os.environ.get("HEAD_MOVES") == "1"
    mode = os.environ.get("NOTPROVEN", "optimistic")
    stats = collections.Counter()
    rows = []
    days = os.environ.get("DAYS", "120")
    cl = subprocess.run(["find", os.path.expanduser("~/.claude/projects"), "-name", "*.jsonl", "-mtime", "-" + days,
                         "-not", "-path", "*/subagents/*", "-not", "-path", "*prog-proof-scratch*"], capture_output=True, text=True).stdout.split()
    for f in cl:
        CURRENT[0] = f
        cwd, ev = claude_events(f)
        if any(e[0] == "STOP" for e in ev):
            stats["claude", "sessions"] += 1
        simulate(cwd, ev, stats, rows, "claude", head_moves, mode)
    cx = sorted(glob.glob(os.path.expanduser("~/.codex/sessions/**/*.jsonl"), recursive=True))
    for f in cx:
        CURRENT[0] = f
        cwd, ev, top = codex_events(f)
        label = "codex" if top else "codex_sub"
        if any(e[0] == "STOP" for e in ev):
            stats[label, "sessions"] += 1
        simulate(cwd, ev, stats, rows, label, head_moves, mode)
    # Per-turn rows include message text: written only when ROWS names a local file.
    if os.environ.get("ROWS"):
        json.dump(rows, open(os.environ["ROWS"], "w"))
    for label in ("claude", "codex", "codex_sub"):
        print("==", label, "HEAD_MOVES=%s NOTPROVEN=%s" % (head_moves, mode))
        ks = sorted(k for (l, k) in stats if l == label)
        for k in ks:
            if not k.startswith(("nc:", "uj:", "parsed:")):
                print("  %-45s %d" % (k, stats[label, k]))
        top_nc = sorted(((stats[label, k], k) for k in ks if k.startswith(("nc:", "uj:", "parsed:"))), reverse=True)[:14]
        print("  ", top_nc)


if __name__ == "__main__":
    main()
