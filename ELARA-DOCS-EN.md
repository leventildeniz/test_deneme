# ELARA — Project Documentation (English)

> Version: 2026-07-06 · Owner: Levent 
> Codename: **ELARA** — a personal, local-first, AI-powered operations platform.

---

## 1. What Did We Build? (Vision, Journey, Current State)

### 1.1 Purpose
ELARA lets the user describe work in natural language **without manually writing any tool/agent/skill**. The system proposes a **compound plan** (tool + skill + agent + capability pack), the user **approves**, and Elara forges + runs it. **The user is an approver, not a coder.**

North-Star examples:
- "Compare iPhone prices" → `web_search + price_scrape + compare_table`
- "Scan xxx.com for vulnerabilities" → `nmap + nikto + sslyze + vuln_report`
- "Connect to this firewall and push config" → `ssh_connect + fw_config_push + fw_operator agent`
- "Extract Instagram trends" → `insta_fetch + trend_analyze + report skill`

### 1.2 How We Got Here (short timeline)
1. **Foundations**: TanStack Start + Bun middleware (`local-server/`) + PostgreSQL + MLX (Apple-Silicon local LLM) + Python embed worker.
2. **RAG pipeline**: bge-m3 dense + FTS hybrid + bge-reranker-v2-m3 (multilingual) + Anthropic-style **contextual enrichment** (each chunk carries brand + version + title preamble).
3. **Agent layer**: `agents/` disk-first Python agents (NetSec ×15, SocialMedia ×10, Meta/forge_master). Shared runner `_shared/mlx_runner.py`.
4. **Tool/Skill layer**: `tools/` on disk (allowlist, stdin/stdout JSON). `skills/` DB-first (LLM prompt bodies).
5. **Capability Registry**: single source of truth at `/system-engine`.
6. **UI = Single Source of Truth**: All prompts and sampling knobs live in the RAG panel. Hidden backend dicts/regex are **forbidden**.
7. **MCP (Model Context Protocol)**: server (expose tools outward) + client (bind external MCP to agents) shipped, with badges.
8. **Meta-Forge**: Elara creates her own tools/agents/skills. Turn-1 cold-classifier bug fixed (semantic anchor retry + orchestrate safety-net).
9. **Approval layer**: Meta-Forge plans listed at `/system-engine → Meta-Forge` with approve/reject/rollback.

### 1.3 Where We Stopped (2026-07-06)
- ✅ Meta-Forge forges + applies **single-capability** plans.
- ✅ Approval UI, rollback button, two-layer confirm (`forge_preview` + `forge_run_prompt`).
- ✅ Dead `capability/*` proposal path fully removed (backend + UI).
- ✅ Chat delta race hardened (empty placeholder bug).
- ✅ Idempotency hash gate on both Meta-Forge entry paths (orchestrate + stream).
- ❌ **Compound proposal** (the true North Star).
- ❌ **Auto-execute after approval**.
- ❌ **Internet-native execution helper** (`tools/_shared/http.py`).
- ❌ **Workflow / Chain builder** (DAG canvas).

### 1.4 Approved Roadmap
- **Phase A — Compound + Auto-Run** (3 sub-turns)
  - A1: Planner "compound intent" mode + JSON schema (`{needs, missing, reuse}`)
  - A2: Approval UI "Approve & Run" — approve + auto-execute + stream to chat
  - A3: `tools/_shared/http.py` (requests + playwright fallback, Mac native network) + smoke
- **Phase B — Workflow / Chain Builder** (DAG, canvas, triggers)
- **Phase C — Idempotency & dedup cleanup** (leftovers, after A+B)

---

## 2. Topology and Architecture

### 2.1 Physical Topology
```
┌────────────────────────────────────────────────────────────┐
│ Mac (M5 Max, 128 GB unified memory)                         │
│                                                              │
│  ┌──────────────┐  ┌─────────────────┐  ┌───────────────┐   │
│  │ Vite dev     │  │ Bun Middleware  │  │ PostgreSQL    │   │
│  │ (UI, TanStack│◄─┤ local-server/   │◄─┤ pgvector      │   │
│  │  Start)      │  │ port 3005       │  │ port 5432     │   │
│  └──────▲───────┘  └───────▲─────────┘  └───────────────┘   │
│         │                  │                                 │
│         │            ┌─────┴─────────┐   ┌────────────────┐  │
│         │            │ MLX Server    │   │ Embed Worker   │  │
│         │            │ (Python)      │   │ (bge-m3 +      │  │
│         │            │ port 8001     │   │  reranker)     │  │
│         │            │ 72B/32B/27B   │   │ port 3006      │  │
│         │            └───────────────┘   └────────────────┘  │
│         │                                                    │
│         │   ┌──────────────────────────────────────────┐    │
│         └───┤ launchd (com.elara.middleware/vite/pg)   │    │
│             └──────────────────────────────────────────┘    │
└────────────────────────────────────────────────────────────┘
        │ (Mac native network — no proxy / gateway)
        ▼
   Internet (web fetch, external MCP servers, APIs)
```

### 2.2 Logical Layers
```
┌────────────────────────────────────────────────────────────┐
│  UI (React 19 + TanStack Start + Tailwind v4)               │
│   /chat  /knowledge  /system-engine  /meta-forge  /workflows│
├────────────────────────────────────────────────────────────┤
│  Middleware (Bun, server.mjs → lib/routes/*)                │
│   • Auth (session gate) • RBAC • Mutation guard             │
│   • Chat orchestrate + stream (SSE)                         │
│   • RAG orchestration (probe → rewrite → retrieve → rerank) │
│   • Agent bridge (spawn Python agent, stream stdout)        │
│   • Meta-Forge (plan → apply → refresh capabilities)        │
│   • MCP server/client, Capability registry                  │
├────────────────────────────────────────────────────────────┤
│  Backends (child processes)                                 │
│   • MLX host (chat completions, embeddings)                 │
│   • Embed worker (bge-m3 embed + bge-reranker-v2-m3)        │
│   • Python agents (spawn-on-demand, stdout bridged to SSE)  │
├────────────────────────────────────────────────────────────┤
│  Storage                                                    │
│   • Postgres: knowledge_chunks (pgvector) + agents/tools/   │
│     skills/capabilities/forge_plans/audit_chain/mcp_*       │
│   • Disk: agents/ tools/ skills/ knowledge sources          │
│   • ~/.elara/state/*.json (brand-aliases, runtime state)    │
└────────────────────────────────────────────────────────────┘
```

### 2.3 End-to-End Chat Flow
```
User prompt
   ▼
UI /chat → POST /api/chat/orchestrate (SSE)
   ▼
[1] Intent classifier (semantic anchors + LLM adjudicator)
    → smalltalk | rag | meta | meta_forge | agent_manifest
   ▼
[2] Lane selection
    ├── smalltalk  → direct MLX (free-answer)
    ├── rag        → probe → denoise → HyDE → vector+FTS →
    │                rerank → inspector directive → MLX stream
    ├── agent      → agent-bridge → spawn python agent
    ├── meta_forge → Meta/forge_master.py → ForgePlan JSON →
    │                idempotency gate → apply → refresh caps
    └── meta       → agents-manifest introduction
   ▼
SSE frames: delta / rag.hit / forge_plan / forge_preview /
            tool_call / agent_done / rag.fallback / done
   ▼
UI render (chat.tsx delta buffer + inline cards)
```

---

## 3. File Map (High-Level + Low-Level)

### 3.1 High-Level
| Path | Purpose |
|---|---|
| `src/routes/` | UI routes (TanStack file-based) |
| `src/components/` | Reusable UI parts |
| `src/lib/` | Client stores + helpers |
| `local-server/server.mjs` | Bun middleware boot |
| `local-server/lib/routes/` | Modular HTTP endpoints |
| `local-server/lib/rag/` | RAG pipeline |
| `local-server/lib/meta-forge/` | Elara self-authoring |
| `local-server/lib/mcp/` | MCP server + client |
| `local-server/lib/agents/` | Agent spawn / env / RAG bridge |
| `local-server/migrations/` | SQL migrations |
| `local-server/scripts/` | CLI smoke / debug / kickstart |
| `agents/` | Python agents (NetSec, SocialMedia, Meta) |
| `tools/` | Python tool implementations |
| `skills/` | Reserved (skill bodies live in DB) |
| `mem/` | Persistent project memory (rules) |

### 3.2 Low-Level (critical files)

**UI**
- `src/routes/__root.tsx` — root layout, head metadata
- `src/routes/_app.tsx` — authed layout (sidebar + top-bar)
- `src/routes/_app.chat.tsx` — main chat (SSE consumer, delta buffer, inline forge/tool cards)
- `src/routes/_app.knowledge.tsx` — library + **RAG panel** (all knobs)
- `src/routes/_app.system-engine.tsx` — Capabilities, Agents, Runtime Safety, Meta-Forge log
- `src/routes/_app.meta-forge.tsx` — plan list + approve/reject/rollback
- `src/routes/_app.tools.tsx` / `.skills.tsx` / `.agents.tsx` — CRUD editors
- `src/routes/_app.mcp.tsx` — MCP exposures + client servers
- `src/routes/_app.workflows.tsx` — Phase B shell (canvas TBD)
- `src/routes/_app.forge.tsx` — classic Action Library editor (distinct from meta-forge)

**Middleware — Chat/RAG**
- `local-server/server.mjs` — express app, boot, migrate, spawn child procs
- `local-server/lib/routes/chat-orchestrate.mjs` — orchestrate lane
- `local-server/lib/routes/chat-stream.mjs` — pure stream lane
- `local-server/lib/rag/intent-classifier.mjs` — semantic + LLM adjudicator
- `local-server/lib/rag/*.mjs` — probe, rewrite, retrieve, rerank, defaults
- `local-server/lib/mlx-transport.mjs` — single MLX transport (state machine, self-heal, invariants)
- `local-server/lib/mlx-queue.mjs` — single-flight queue

**Middleware — Meta-Forge**
- `local-server/lib/meta-forge/planner.mjs` — plan schema + inventory
- `local-server/lib/meta-forge/apply.mjs` — plan → disk + DB
- `local-server/lib/meta-forge/refresh.mjs` — capabilities re-sync
- `local-server/lib/meta-forge/idempotency.mjs` — hash gate
- `local-server/lib/routes/meta-forge.mjs` — HTTP endpoints

**Middleware — MCP**
- `local-server/lib/mcp/*.mjs` — JSON-RPC 2.0 core
- `local-server/lib/mcp/client.mjs` — client for external MCP servers
- `local-server/lib/routes/mcp.mjs` — `/mcp` + admin API

**Middleware — Agent lane**
- `local-server/lib/agent-bridge.mjs` — spawn + stdout SSE
- `local-server/lib/agent-env.mjs` — inject `ELARA_AGENT_TOOLS` manifest
- `local-server/lib/agent-rag.mjs` — agent-side RAG fetcher (data only)
- `local-server/lib/agents-manifest.mjs` — dynamic agent introduction

**Python**
- `agents/_shared/mlx_runner.py` — shared runner (chat template, streaming)
- `agents/_shared/dispatch.py` — `call_tool()` loopback dispatch
- `agents/_shared/config_center.py` — env → tools/sources block
- `agents/Meta/forge_master.py` — Meta-Forge planner agent
- `agents/NetSec/*.py` — 15 network-security specialists
- `agents/SocialMedia/*.py` — 10 social-media roles

**DB**
- `local-server/schema.sql` — root schema
- `local-server/migrations/*.sql` — ordered migrations
- Boot-time DDL self-heal in `lib/db.mjs` + `lib/migrate.mjs`

---

## 4. Design Details: RAG, Agents, Workflow

### 4.1 RAG Topology
```
Query
  ▼
[Intent classifier] → smalltalk skips RAG
  ▼
[Pre-RAG deadline gate] (default 6s, timeout → free-answer fallback)
  ▼
[Probe] — bge-m3 dense, HNSW top-k
  │   • perSourceCap = 3
  │   • perBrandCap  = 6
  │   • diversityPool = 200
  │   • minChunkChars = 100
  ▼
[Decision] probe.top1 vs injectThreshold
  ├── < threshold → strict gate (skip FTS-only + reranker)
  └── ≥ threshold → continue
  ▼
[Denoise + Rewrite] LLM query cleanup (typos, smalltalk residue)
  ▼
[HyDE expand] topic expansion (vendor-agnostic)
  ▼
[Retrieve] vector + FTS hybrid, RRF fusion
  │   coverage = max(content_hits, 0.5*metadata_hits)
  ▼
[Rerank] bge-reranker-v2-m3 (multilingual XLM-R on MPS)
  ▼
[Confidence gate] rerankSafe OR coverageSafe OR brandSafe → inject
  ▼
[Dominant Brand Lock] rows ≥70% single vendor → prompt gets Rule 6
  ▼
[Inspector directive] system prompt + source references
  ▼
MLX stream
```

**Contextual Enrichment (Anthropic-style)**: `enrich-structured-chunks.mjs` prepends every chunk with `Brand + Version + Title`. The embed vector naturally carries version/brand tokens — no static regex or whitelist needed.

**Library-aware free-answer**: When there's no RAG hit:
- `in-library miss` (brand exists, no context) → "I have X in my library but no context"
- `out-of-library` (top-5 scope stated) → "library scope is A,B,C; answering from model knowledge"

### 4.2 Agent Architecture
```
User → Chat → intent=agent → agent-bridge
  ▼
spawn python <agent.py>  (env: ELARA_AGENT_MODEL,
                                ELARA_AGENT_TOOLS,
                                ELARA_AGENT_SOURCES,
                                PROMPT)
  ▼
agents/_shared/mlx_runner.py
  ├── chat template (qwen2.5 / chatml / llama3 / gemma4)
  ├── streaming stdout → SSE proxy
  └── !<tool_slug>({json}) parser (post-stream)
       ▼
       POST /api/agents/tool-call (loopback, manifest gate)
         ▼
         tools/<slug>.py  stdin JSON → stdout JSON
         ▼
       SSE tool_call event → UI ToolTrace card
```

**Squad orchestration** (`agents/*/orchestrator.py`): intra-squad coordination; audit-chain written automatically.

**Meta agent (Meta/forge_master.py)**: reads inventory → LLM plan → `POST /api/meta-forge/plans` → admin apply.

### 4.3 Workflow (Phase B — not yet implemented)
Planned:
- `workflow_defs` table (JSON DAG: nodes + edges)
- Node types: `tool.call`, `agent.spawn`, `skill.render`, `branch`, `parallel`, `retry`
- Triggers: manual / cron (pg_cron) / webhook (`/api/public/webhook/*`)
- Canvas at `/workflows` (React Flow-like library under evaluation)

### 4.4 Tool / Skill / Capability
- **Tool** — disk-first (`tools/<slug>.py`), stdin/stdout JSON, allowlist gates, contract JSON at `local-server/tools/contracts/`.
- **Skill** — DB-first (`skills` table), body = LLM system prompt fragment. `skills/` folder is reserved.
- **Capability** — linked by slug. `capability_packs` = sectoral themes (NetSec pack, SocialMedia pack).
- **Registry UI** — `/system-engine → Capabilities`, admin-only, with "Re-sync from sources" button.

---

## 5. Roadmap & Recent Issues

### 5.1 Roadmap (approved)
| Phase | Task | Status |
|-------|------|--------|
| A1 | Compound proposal JSON schema + planner mode | Pending |
| A2 | Approval UI "Approve & Run" + auto-execute | Pending |
| A3 | `tools/_shared/http.py` (Mac native web) + smoke | Pending |
| B  | Workflow/Chain builder (DAG + canvas) | Pending |
| C  | Idempotency cleanup leftovers | Deferred (after A+B) |

### 5.2 Recent Issues (chronological)
1. **Turn-1 Meta-Forge didn't open** (cold classifier + `assistant_meta_text` bypass).
   - Fix: `hasCreationVerb` guard cancels meta-bypass on "create/build/make".
   - Fix: anchor embed retry (budget 3.5s, 250ms steps).
   - Fix: orchestrate safety-net retry when `intentClassifyReason` in cold set.
2. **Chat delta race** — placeholder stayed empty until refresh.
   - Fix: `assistantIdRef` swap + `flushSync` + empty-bubble fallback text.
3. **Idempotency gate didn't fire** — same prompt spawned 3 fresh plans.
   - Cause: gate lived only in orchestrate; agent-bridge Meta-Forge path bypassed it.
   - Fix: gate on both lanes + `intent_hash` stamp aligned in `apply.mjs`.
4. **Dead `capability/*` proposal path** left over after Meta-Forge takeover.
   - Fix: removed 11 files, cleaned wiring, drop migration.
5. **Latency 43–52 s** — accepted (memory-bandwidth limited, GPU 99 %). Model will NOT change.

### 5.3 Open Items
- Compound proposal (Phase A — the architectural leap).
- Auto-execute after approval.
- Internet-native tool helper.
- Workflow DAG builder.

---

## 6. Menu Guide (What Each Page Does)

| Menu | Route | Purpose |
|------|-------|---------|
| **Dashboard** | `/dashboard` | System health, recent runs, shortcuts |
| **Chat** | `/chat` | Main chat. Prefixes: `@`=agent, `!`=skill, `/`=tool |
| **Knowledge** | `/knowledge` | Library management + **RAG panel** (single source of truth for knobs) |
| **Knowledge → Aliases** | `/knowledge/aliases` | Brand aliases (UI-managed; editing JSON directly is forbidden) |
| **Capabilities** | `/capabilities` | Registered capability slugs & tags |
| **Agents** | `/agents` | Python agent CRUD (disk-first), header sidecar |
| **Tools** | `/tools` | Tool contract editor, brain picker per tool |
| **Skills** | `/skills` | Skill body editor (LLM prompt), DB-first |
| **Forge** | `/forge` | Classic Action Library editor (distinct from meta-forge) |
| **Meta-Forge** | `/meta-forge` | Elara-proposed plans + approve/reject/rollback |
| **MCP** | `/mcp` | Server exposures + client (external MCP) config |
| **Workflows** | `/workflows` | Phase B shell (DAG canvas coming) |
| **Orchestration** | `/orchestration` | Squad coordination view |
| **Planner** | `/planner` | LLM planner helper (test tool) |
| **Approvals** | `/approvals` | Central pending-approvals list |
| **Models** | `/models` | Model registry (MLX + cloud transports) |
| **Adapters** | `/adapters` | Vendor adapter dictionaries (RADIUS etc.) |
| **Middleware** | `/middleware` | Middleware/worker status, restart buttons |
| **System-Engine** | `/system-engine` | Capabilities re-sync, Runtime Safety, Meta-Forge log, Agents scan |
| **Policies** | `/policies` | RBAC + execution policies |
| **Users** | `/users` | User management (Admin) |
| **Security** | `/security` | Security audit / scan view |
| **CVE** | `/cve` | CVE watcher list |
| **Reports** | `/reports` | PDF report generator |
| **Templates** | `/templates` | Prompt/template management |
| **Targets** | `/targets` | Target inventory (FWs, hosts, etc.) |
| **Live Call** | `/live-call` | Live audio/WS test tool |
| **Python** | `/python` | Python interpreter registry |
| **Telemetry** | `/telemetry` | Live telemetry (TTFT, tok/s, RAG ms) |
| **Debug** | `/debug` | Debug live stream (SSE audit ticker) |
| **Settings** | `/settings` | General settings |

---

## Appendix — Immutable Rules
1. **UI is the single source of truth** — every prompt/sampling/knob lives in the RAG panel. Hidden backend dicts/regex are forbidden.
2. **Everything dynamic** — static whitelists / seed JSON / hand-picked brand lists are forbidden.
3. **Plan-first** — no code ships without approval.
4. **Mac native network** — proxy/API-gateway wrappers are forbidden.
5. **User = approver**, not coder.
6. **AI = teammate**: dissent is welcome, blind compliance is not.

_Last updated: 2026-07-06 · Source: `mem://roadmap/elara-north-star-2026-07-06.md`._
