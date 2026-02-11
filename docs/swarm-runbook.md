# FrankenTerm Swarm Runbook

One-page operating guide for many agents and many panes.

This document is intentionally complementary to `docs/swarm-playbook.md`.
- `docs/swarm-playbook.md`: canonical Robot/MCP operating loop and command contract.
- `docs/swarm-runbook.md` (this file): team operating model, naming, ownership, and ultra-simple wrappers.

## 0) Ultra-Simple Mode (recommended)

If full `ft robot ...` feels too heavy, use the wrappers:

```bash
source /Users/sd/projects/joyshmitz/frankenterm/scripts/ft-easy-aliases.sh
```

Then operate with 5 commands only:
- `frstart` -> start watcher with auto-handle
- `frnow` -> show state + unhandled events
- `frtail <pane_id> [lines]` -> show pane context
- `frsend <pane_id> "<cmd>" [pattern] [timeout]` -> dry-run + execute + verify
- `frfix <pane_id> [cmd] [pattern] [timeout]` -> diagnose pane, optionally execute fix

For the full canonical Robot/MCP loop, use `docs/swarm-playbook.md`.

## 1) Naming Convention

Use stable IDs so people and agents can coordinate without confusion.

- `workspace`: `<stream>-<area>`  
  Example: `sw-core`, `sw-fe`, `sw-infra`
- `agent_id`: `<team>-<role>-<n>`  
  Example: `core-impl-1`, `core-test-2`, `infra-fix-1`
- `task_id`: `<board>-<ticket>`  
  Example: `ft-142`, `ops-77`
- `reason`: `<task_id>: <short action>`

## 2) Pane Bookmark Policy

Every active pane gets an alias immediately.

```bash
ft panes bookmark add <pane_id> --alias core-impl-1
ft panes bookmark add <pane_id> --alias core-test-2
ft panes bookmark add <pane_id> --alias infra-fix-1
ft panes bookmark list
```

Rules:
- Alias must match `agent_id`.
- Never work in an unbookmarked pane.

## 3) Reservation Policy

Before any mutating action, reserve pane. After work, release it.

Reserve:
```bash
ft robot --format toon reservations reserve <pane_id> \
  --owner-id <agent_id> \
  --owner-kind agent \
  --ttl 1800 \
  --reason "<task_id>: implement fix"
```

List:
```bash
ft robot --format toon reservations list
```

Release:
```bash
ft robot --format toon reservations release <reservation_id>
```

Rules:
- One agent = one active reservation.
- TTL refresh or re-reserve if work exceeds 30 min.

## 4) Daily Control Loop (Operator)

Run watcher once:
```bash
ft watch --auto-handle --foreground
```

Then follow the canonical loop from `docs/swarm-playbook.md`.

Operator responsibility in this runbook:
1. Keep aliases/bookmarks consistent.
2. Enforce reservations before mutating actions.
3. Prioritize incidents by severity.
4. Ensure clean handoff between shifts.

## 5) Incident Priority

Process in this order:
1. `policy_denied`, stuck prompt, auth blocks
2. build/test failures in critical branch
3. usage/rate limit events
4. warnings/noise

## 6) Standard Playbooks (Operational)

### A) Build/Test failure
Use canonical loop from `docs/swarm-playbook.md`; add this operator rule:
- annotate/triage the event before reassignment.

### B) Agent appears stuck
Use canonical loop from `docs/swarm-playbook.md`; add this operator rule:
- if stuck > 10 min, reassign with updated `owner_id` and reason.

### C) Usage limit handling
Use canonical loop from `docs/swarm-playbook.md`; add this operator rule:
- mark as capacity incident and log next reset window in handoff.

## 7) Shift Handoff Template

Use this exact summary:

```text
Swarm summary:
- Active panes: <count>
- Unhandled events: <count>
- Critical incidents: <ids>

Per agent:
- <agent_id> | pane <id> | task <task_id> | state <running/blocked/done>
- Last action: <command>
- Next action: <command or decision>

Reservations:
- <reservation_id> | <agent_id> | pane <id> | expires <ts>
```

## 8) Source of Truth

- Canonical Robot/MCP flow: `docs/swarm-playbook.md`
- Wrapper commands and team conventions: this file
