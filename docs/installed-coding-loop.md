# Installed coding-loop smoke

This deterministic check exercises the shipped portable Agent Skill and its
exact-argv wrapper with a real Rust test project. It uses a copy of a built or
installed `prog` binary, a temporary project outside the source checkout, a
dedicated store and Cargo target directory, and isolated Git configuration.
It requires Python 3, Git, Cargo, and a working Rust toolchain.

```sh
cargo test -p prog-cli --test installed_coding_loop
```

To test a particular executable and save the complete JSON report:

```sh
python3 fixtures/harness/installed_coding_loop.py \
  --prog /path/to/prog --output /tmp/prog-installed-loop.json
```

The driver performs this workflow:

1. Install and verify the portable host files with `harness install` and
   `harness doctor`.
2. Run a noisy, failing Cargo library suite through the installed wrapper.
   Check the original exit code, argv, cwd, and environment.
3. Select the actual Rust panic returned by `inspect`, retrieve its evidence,
   follow any excerpt omissions with an exact cached export, and verify both
   the export receipt and the reference's slice hash.
4. Apply the fixture's known source correction externally, then run the same
   suite again and require a fresh observation.
5. Declare the fixture caller's fixed criterion against that evidence. Check
   that `status` reports a passed obligation and the same comparability and
   delta counts as the canonical commands. Retrieve the original evidence
   again to prove its cursor still addresses the earlier failure.

Negative controls check that a narrower successful test cannot prove the
failure absent, a later workspace edit invalidates an earlier pass, and a
successful command with truncated capture cannot satisfy readiness. Evicting
the full-suite evidence also invalidates a previously passing obligation. Test-side
execution records establish that navigation and verification do not rerun the
suite. Each negative control has a separate fixture session; it never changes
the successful session's criterion.

The report contains the actual argv, exit code, stdout, stderr, and byte counts
for every invocation, plus bytes read from cached exports. Setup commands are
included and labelled. It also records external source edits, observation
identifiers, the exact failure slice, successful status, and blocking negative
controls. Temporary projects and stores are removed after the check; the report
retains the captured receipts and response bytes.

This is an installed-workflow regression test. Its source correction is known
to the fixture driver. It does not measure agent problem-solving, register a
three-tool facade, make provider calls, or establish a context-cost advantage.
The installed instruction file's size is labelled as available surface;
delivered model context requires an actual host/agent trial. The measured
facade comparison now reuses this fixture through the
[real registered host](registered-host-facade.md). Actual-agent trials remain
part of #120 and #139.
