# FrankenTerm Swarm Runbook

One-page operating guide for many agents and many panes.

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

Then repeat this loop:
1. Map current swarm:
```bash
ft robot --format toon state
```
2. Pull incident queue:
```bash
ft robot --format toon events --unhandled --limit 50
```
3. Pick top incident (highest risk first).
4. Open context:
```bash
ft robot --format toon get-text <pane_id> --tail 150
```
5. Execute safe action:
```bash
ft robot --format toon send <pane_id> "<cmd>" --dry-run
ft robot --format toon send <pane_id> "<cmd>" --wait-for "<pattern>" --timeout-secs 600
```
6. Verify result via `events` and `search`.
7. Release reservation.

## 5) Incident Priority

Process in this order:
1. `policy_denied`, stuck prompt, auth blocks
2. build/test failures in critical branch
3. usage/rate limit events
4. warnings/noise

## 6) Standard Playbooks

### A) Build/Test failure
```bash
ft robot --format toon search "error OR panic OR failed" --pane <pane_id> --limit 20
ft robot --format toon send <pane_id> "cargo test -- --nocapture" --wait-for "test result:" --timeout-secs 1800
```

### B) Agent appears stuck
```bash
ft robot --format toon get-text <pane_id> --tail 200
ft robot --format toon wait-for <pane_id> ">" --timeout-secs 60
```
If no progress, send controlled nudge:
```bash
ft robot --format toon send <pane_id> "status" --wait-for "status" --timeout-secs 120
```

### C) Usage limit handling
```bash
ft robot --format toon workflow run handle_usage_limits <pane_id>
```

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

## 8) Minimal Command Set (memorize)

- `ft robot --format toon state`
- `ft robot --format toon events --unhandled --limit 50`
- `ft robot --format toon get-text <pane_id> --tail 150`
- `ft robot --format toon send <pane_id> "<cmd>" --dry-run`
- `ft robot --format toon send <pane_id> "<cmd>" --wait-for "<pattern>" --timeout-secs 600`
- `ft robot --format toon reservations reserve ...`
- `ft robot --format toon reservations release <reservation_id>`
