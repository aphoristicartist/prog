#!/usr/bin/env python3
import argparse
import json
import pathlib
import subprocess
import tempfile


def approx_tokens(byte_count):
    return (byte_count + 3) // 4


def run(args):
    completed = subprocess.run(args, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if completed.stderr:
        raise SystemExit(completed.stderr.decode())
    return completed.stdout


def main():
    parser = argparse.ArgumentParser(
        description="Measure optional external payload captures with prog observe/expand."
    )
    parser.add_argument(
        "payload",
        nargs="+",
        help="LABEL:MIME:PATH:JSON_POINTER, for example github:application/json:/tmp/pr.json:/review_threads/0",
    )
    parser.add_argument("--prog", default="prog")
    parser.add_argument("--dir", default=None)
    args = parser.parse_args()

    temp = tempfile.TemporaryDirectory() if args.dir is None else None
    prog_dir = args.dir or temp.name
    rows = []
    for spec in args.payload:
        label, mime, path, pointer = spec.split(":", 3)
        payload_path = pathlib.Path(path)
        raw_bytes = payload_path.read_bytes()
        observed = json.loads(
            run(
                [
                    args.prog,
                    "--dir",
                    prog_dir,
                    "observe",
                    "--file",
                    str(payload_path),
                    "--mime",
                    mime,
                    "--name",
                    label,
                ]
            )
        )
        expanded = json.loads(
            run([args.prog, "--dir", prog_dir, "expand", observed["cursor"], "--path", pointer])
        )
        rows.append(
            {
                "label": label,
                "raw_bytes": len(raw_bytes),
                "observe_envelope_bytes": observed["summary"]["envelope_bytes"],
                "expand_envelope_bytes": expanded["summary"]["envelope_bytes"],
                "token_ratio": round(
                    approx_tokens(len(raw_bytes))
                    / max(1, approx_tokens(len(json.dumps(observed)) + len(json.dumps(expanded)))),
                    2,
                ),
            }
        )

    print("| Payload | Raw bytes | observe envelope bytes | expand envelope bytes | Token ratio |")
    print("|---|---:|---:|---:|---:|")
    for row in rows:
        print(
            f"| {row['label']} | {row['raw_bytes']} | {row['observe_envelope_bytes']} | "
            f"{row['expand_envelope_bytes']} | {row['token_ratio']}x |"
        )


if __name__ == "__main__":
    main()
