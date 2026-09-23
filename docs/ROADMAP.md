# Roadmap — ferro vs px0

Order: 1. local speed, 2. built-in agent, 3. team/enterprise.

## P1 local speed (now)
- [x] scaffold: index, fuzzy, search, viewer, git diff
- [ ] windowed highlight (syntect, 1000-line window + 400 ctx)
- [ ] virtual rows for 400k-line files
- [ ] persistent index cache (sqlite, mtime+size invalidation)
- [ ] `benchmark.sh` parity table vs px0

Win condition: `ferro ~/.cache/bench/linux` indexes <400ms, fuzzy <7ms, RSS <25MB idle.

## P2 built-in agent (next)
px0 dispatches to external CLIs. Ferro runs its own sandboxed agent:
- tool allowlist: read/write/destructive labels
- patch apply with git snapshot + rollback
- session log as Markdown in `.ferro/`
- providers: Anthropic/OpenAI/Ollama via env key, no vendor lock

## P3 team/enterprise
- TLS + OIDC, audit log
- shared PR reviews (Postgres), inline comments
- MCP server: expose files/search/diff as tools
- GitHub/GitLab/Gitea forge abstraction
