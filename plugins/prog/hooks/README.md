# Hooks

No transparent Codex shell hook is registered. Codex supplies shell input as a
single command string, so rebuilding argv would violate prog's exact-command
contract. The bundled skill and `scripts/prog-run.sh` are the safe integration.
