# Cache behavior

`prog call` stores read-only, cacheable, non-sensitive responses in the local store under `--dir`. `prog expand` reads those cached payloads by cursor and never contacts the upstream source.

## TTLs

Profiles and operations can define a `cache` policy with `enabled`, `ttl_seconds`, and `refresh_after_seconds`. If no policy is enabled but the operation effects say `cacheable: true` and `sensitive: false`, `prog` enables a default TTL of 86400 seconds.

The envelope reports cache state:

```json
{
  "cache": {
    "status": "stored",
    "ttl_seconds": 86400,
    "expires_at": "2026-07-05T21:50:56Z",
    "age_seconds": 0
  }
}
```

Expired entries are treated as unavailable. Re-run the original call to refresh them.

## Refresh and bypass

Use `--refresh` when you want a new upstream call even if a cached entry exists:

```bash
prog --dir /tmp/prog-demo call demo_cli list --args '{}' --refresh
```

Use `--no-cache` to execute the upstream call but skip persistence:

```bash
prog --dir /tmp/prog-demo call demo_cli list --args '{}' --no-cache
```

`--no-cache` also means no reusable cursor can be created for later offline expansion.

## Staleness warnings

`prog expand` reports the age of the cached payload when it is older than the current second:

```text
cached payload age_seconds=7; re-run `prog call demo_cli list --refresh` to refresh
```

The warning is informational. Expansion still uses the exact cached payload associated with the cursor.

## Purge behavior

List entries:

```bash
prog --dir /tmp/prog-demo cache list
```

Read one entry by cache key:

```bash
prog --dir /tmp/prog-demo cache get sha256:e29b6ba3f44898f322fc681bb5246e17bcbce606816e89be34379435f341c6cd
```

Purge all cache state:

```bash
prog --dir /tmp/prog-demo cache purge --all
```

Purge expired entries:

```bash
prog --dir /tmp/prog-demo cache purge --expired
```

Purge all entries for one source:

```bash
prog --dir /tmp/prog-demo cache purge --source demo_cli
```

Purging entries also removes their payloads and cascades to cursors that point at those entries. After purge, old cursors fail closed with a cache-miss or cursor-not-found JSON error.

## What to commit

Profiles under `--dir/profiles` are intentionally readable JSON and can be committed when they describe stable local sources. Cache entries, payloads, cursors, and temporary output files are runtime state and should not be committed.
