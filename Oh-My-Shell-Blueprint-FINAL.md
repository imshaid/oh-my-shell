# Oh My Shell — Project Blueprint

**Course:** CSE324 — Operating Systems Lab (Complex Engineering Problem, Team Project)
**Team Size:** 5
**Total Marks:** 40

---

## ১. Executive Summary

**Oh My Shell** একটা AI-পাওয়ার্ড, natural-language Linux shell। User plain English-এ একটা request টাইপ করলে, শেল সেটা বুঝে একটা step-by-step plan দেখাবে, user সেই plan নিয়ে discuss/adjust করতে পারবে, আর confirm করার পর একটা live streaming interface-এ সেটা execute হবে। User চাইলে exact শেল command-ও সরাসরি চালাতে পারবে — সেক্ষেত্রে AI silent থাকবে, তবে কোনো command destructive মনে হলে intervene করে explanation ও confirmation চাইবে।

**Architecture clarification:** Oh My Shell একটা standalone, login-shell-capable executable (`bash`/`zsh`-এর সমতুল্য একটা shell program) — এটা কোনো নতুন terminal emulator (GNOME Terminal/xterm-এর বিকল্প) না। এটা system-এর existing terminal infrastructure (pty, rendering, font-handling) ব্যবহার করে চলে, ঠিক যেভাবে যেকোনো শেল চলে। ইউজার এটাকে `/etc/shells`-এ নিবন্ধিত একটা independent shell হিসেবে ব্যবহার করতে পারবে (default shell হিসেবে সেট করা সম্ভব, `chsh` দিয়ে), অথবা existing bash-এর ভেতর থেকে `oh-my-shell` কমান্ড দিয়ে চালু করতে পারবে। এটা কোনো অন্য শেলের (zsh, fish) plugin/extension হিসেবে কাজ করে না — এটা নিজেই একটা independent, dedicated shell।

এই প্রজেক্ট কোর্সের mandatory shell-scripting requirement পূরণ করে (Bash-ই core execution layer), সাথে একটা modern, AI-driven interaction layer যোগ করে।

---

## ২. Problem Statement এবং Complex Problem Definition

### ২.১ Core Problem

Linux শেল effectively ব্যবহার করতে exact syntax (flags, pipes, regex) মুখস্থ রাখতে হয় — এটা নতুন এবং experienced, দুই ধরনের user-এর জন্যই একটা persistent friction। অন্যদিকে, free-form natural language থেকে সরাসরি AI-generated command auto-execute করা unsafe, যদি কোনো proper safety layer না থাকে।

### ২.২ কেন এটা একটা Complex Engineering Problem

- **Depth of Knowledge:** এই প্রজেক্টে shell scripting, process ও permission management, file-system operations, এবং LLM/AI integration — এই কয়েকটা আলাদা domain একসাথে combine করতে হয়, যেটা কোনো একটা single কোর্স টপিকের মধ্যে পড়ে না।
- **Project Context:** এটা একটা practical, everyday tool যেটা real sysadmin কাজে (file cleanup, process monitoring, permission management) ব্যবহারযোগ্য — শুধু textbook exercise না, বাস্তব ব্যবহারের প্রেক্ষাপটে যায়।

### ২.৩ Conflicting Requirements এবং সমাধান

| Conflict | ব্যাখ্যা | সমাধান |
|---|---|---|
| **Frictionless UX বনাম Safety** | User দ্রুত, বাধাহীনভাবে কাজ করতে চায়, কিন্তু ভুল command সিস্টেম নষ্ট করতে পারে | Risk-tiered intervention — raw command সরাসরি চলবে, শুধু বিপজ্জনক pattern শনাক্ত হলেই AI intervene করবে |
| **Automation বনাম Predictability** | Free-form AI command generation flexible কিন্তু unpredictable | Capability-registry (whitelist)-ভিত্তিক action mapping — কোনো fine-tuning বা arbitrary command generation নেই; risk-level ও registry-তে static/hardcoded, AI-generated না (Section 7.4 দেখুন) |
| **Personalization বনাম Performance/Predictability** | ব্যবহারের সাথে সাথে শেল "শিখবে" এই ধারণাটা আকর্ষণীয়, কিন্তু unbounded "learned rules" store সময়ের সাথে prompt-size ও latency বাড়িয়ে দেয় | Adaptive learning-layer বাদ দেওয়া হয়েছে — এর বদলে একটা comprehensive, static, well-specified Capability Registry ব্যবহার করা হয়, যা predictable ও bounded থাকে (risk-level static রাখার প্রযুক্তিগত যুক্তির জন্য Section 7.4 দেখুন) |
| **Plan Quality বনাম Instant-Feel UX** | Reasoning model উত্তর দেওয়ার আগে chain-of-thought তৈরি করে, যা plan-এর quality বাড়ায় কিন্তু latency যোগ করে | Reasoning trace কখনো raw দেখানো হয় না; "Thinking…" indicator-এ live token-count দেখিয়ে latency-টা intentional মনে করানো হয় |
| **Local-first Predictability বনাম Hardware Accessibility** | সম্পূর্ণ local architecture সবচেয়ে predictable ও privacy-preserving, কিন্তু dGPU-বিহীন হার্ডওয়্যারে ব্যবহারযোগ্যতা কমিয়ে দেয় (Section 7.8-এ hands-on verified) | Local Ollama-ই primary architecture থাকে (development ও primary demo Shaid-এর dGPU-মেশিনে); Google AI Studio API শুধু dGPU-বিহীন team-member-দের জন্য documented, opt-in fallback হিসেবে যোগ করা হয়েছে (Section 7.8) — internet-dependency তৈরি হয় সেই মোডে, যা explicitly note করা আছে |

### ২.৪ Rubric Alignment Note

কোর্সের CEP guideline-এ Engineering Principles (EP1, EP2, EP4) ও Activities (EA1, EA2, EA4)-এর উদাহরণ মূলত network-service (Mail, DNS, DHCP, Web, Proxy, Samba server) দিয়ে দেখানো হয়েছে। তবে guideline-এর মূল requirement হলো একটা প্রজেক্টে নিচের যেকোনো **একটা** থাকলেই চলবে: file systems and access permissions, network and resource sharing, অথবা security and user authentication। Oh My Shell file-system/permission handling পুরোপুরি cover করে, আর security-র অংশও আংশিকভাবে কভার করে (permission escalation, destructive-command detection); rubric coverage আরও broaden করার জন্য একটা lightweight network/security capability set যোগ করা হয়েছে (Section 6, item 12)।

এই alignment instructor-এর সাথে confirm করা এখনো বাকি — দেখুন Section ১৫, Open Question ১।

---

## ৩. Feasibility ও Gap Analysis

**বিদ্যমান সমাধান:** llmshell-cli, ai2cli, shell-ai, cmdai-এর মতো টুল ইতিমধ্যে আছে, যেগুলো natural language থেকে শেল command জেনারেট করে।

**Gap:** এই বিদ্যমান টুলগুলো মূলত (ক) single-shot, non-conversational — plan discuss/adjust করার সুযোগ নেই, (খ) non-streaming — raw output দেখায়, live progress না, (গ) transparent, built-in permission-escalation flow নেই।

**General-purpose AI agent framework (OpenClaw, Hermes Agent) থেকে পার্থক্য:** এই framework-গুলো shell-command execution-কে তাদের ৪০+ tool-এর একটা অংশ হিসেবে সাপোর্ট করে (browser automation, multi-channel messaging, image generation, multi-agent orchestration-সহ) — এগুলো সাধারণ-উদ্দেশ্য automation platform, cloud-model-নির্ভরতা default এবং self-evolving/persistent-learning আচরণসহ। Oh My Shell ইচ্ছাকৃতভাবে scope সংকীর্ণ রাখে:

| দিক | OpenClaw/Hermes-জাতীয় general agent | Oh My Shell |
|---|---|---|
| Design target | সাধারণ-উদ্দেশ্য automation (email, browser, image-gen, ইত্যাদি) | শুধু Linux shell/sysadmin কাজের জন্য purpose-built |
| Model dependency | Cloud model default, local optional | সম্পূর্ণ local-first |
| Safety architecture | কিছু ক্ষেত্রে approval সম্পূর্ণ বন্ধ করার mode আছে | Risk কখনো disable করা যায় না; static hardcoded registry |
| Predictability | Self-evolving/learning loop | Deterministic, static capability set |
| Attack surface | ব্যাপক (৪০+ tool, plugin ecosystem) | ছোট, whitelist-only action-set |

**Feasibility Assessment:**

| Dimension | মূল্যায়ন |
|---|---|
| Technical | ১০টা local LLM, ৪০টা real-life prompt দিয়ে hands-on বেঞ্চমার্ক করে verify করা হয়েছে (Section 7)। ফলাফল অনুযায়ী architecture-এ কয়েকটা গুরুত্বপূর্ণ সংশোধন আনা হয়েছে |
| Resource | টিমের কারো prior local-LLM experience নেই — Week ১-এই hands-on verification প্ল্যান করা হয়েছে (সম্পন্ন — Section 7.2) |
| Schedule | ৫-৬ সপ্তাহের মধ্যে core পাঁচটা module realistic; safety/permission module-এ সবচেয়ে বেশি implementation risk আছে, তাই সেটা conservatively scope করা হয়েছে (Section 9, Module 3) |
| Operational | একটা AI যে destructive command চালাতে পারে, সেটা user কতটা trust করবে — এটাই মূলত Frictionless-UX-vs-Safety conflict (Section 2.3)-এর ভিত্তি, এবং architecture-এই সরাসরি address করা হয়েছে |

---

## ৪. System Architecture

### ৪.১ High-Level Flow

```
[User Input]
      │
      ▼
┌──────────────────┐
│   Input Router    │  (raw shell syntax? / slash command? / natural language?)
└──────────────────┘
      │                │                                    │
  raw shell        slash command                     natural language
      │                │                                    │
      ▼                ▼                                    ▼
┌───────────────┐ ┌──────────────┐                 ┌─────────────────┐
│Danger Classifier│ │Meta-Command  │                 │  Intent Parser   │
└───────────────┘ │  Handler     │                 │  (Local LLM,     │
      │            └──────────────┘                 │  think:false,    │
  safe │  dangerous                                  │  JSON-Schema-    │
      │      ▼                                       │  constrained)    │
      │  ┌──────────────┐                            └─────────────────┘
      │  │ Confirmation  │                                    │
      │  │   Dialog      │                          ┌─────────────────┐
      │  └──────────────┘                            │Harness Validation│
      │      │ confirmed                             │ (Pydantic schema │
      │      │                                        │  double-check)   │
      │      │                                       └─────────────────┘
      │      │                                                │
      │      │                                       ┌─────────────────┐
      │      │                                       │Capability Registry│
      │      │                                       │Lookup (action →   │
      │      │                                       │fixed risk, params  │
      │      │                                       │schema — static)    │
      │      │                                       └─────────────────┘
      │      │                                                │
      │      │                                       ┌─────────────────┐
      │      │                                       │ Plan Generator   │
      │      │                                       └─────────────────┘
      │      │                                                │
      │      │                                       ┌─────────────────┐
      │      │              discuss/adjust           │ Confirmation +   │
      │      │             ◀──────────────────────── │ Discussion Loop  │
      │      │                                       └─────────────────┘
      │      │                                                │ confirmed
      └──────┴────────────────────┬───────────────────────────┘
                                   ▼
                     ┌──────────────────────────────┐
                     │ Sudo/Permission Escalation     │
                     │ Layer (শুধু AI-generated plan-এ  │
                     │  প্রয়োজন হলে সক্রিয়; raw sudo      │
                     │  সরাসরি OS-prompt-এ যায়)          │
                     └──────────────────────────────┘
                                   │
                                   ▼
                     ┌──────────────────────────────┐
                     │      Streaming Executor        │
                     │   (blocking; live progress;     │
                     │    শুধু Ctrl+C interrupt করে)     │
                     └──────────────────────────────┘
                                   │
                                   ▼
                     ┌──────────────────────────────┐
                     │  Collapsed Summary + Audit Log  │
                     └──────────────────────────────┘
```

Raw-command এবং natural-language path — দুটোই execution-এর আগে একই Sudo/Permission Escalation Layer দিয়ে যায় (ইউজার raw command-এ নিজে explicit sudo লিখলে ব্যতিক্রম — Section 8.3.6 দেখুন), যাতে permission handling consistent থাকে। Meta-Command Handler এই merge-point-এ ঢোকে না — slash command সরাসরি respond করে সেশন-লেভেলে শেষ হয় (Section 4.2)।

### ৪.২ Component Breakdown

| Component | দায়িত্ব | Tech Stack | Owner |
|---|---|---|---|
| Input Router | Input raw shell syntax / slash-command / natural language তা classify করে; bracketed-paste detection | Bash, regex/pattern matching | Module 2 |
| Meta-Command Handler | Slash command (`/help`, `/model`, `/undo`, ইত্যাদি) পার্স করে সরাসরি respond করে — Intent Parser/AI pipeline সম্পূর্ণ bypass করে | Bash/Python (dispatch logic) | Module 2 (dispatch), Module 4 (output rendering) |
| Intent Parser | Natural-language input-কে structured JSON intent-এ রূপান্তর করে; JSON-Schema পাঠানো, `think:false` সেট করা; provider-abstraction-এর মাধ্যমে backend সুইচযোগ্য (Ollama default / Google AI Studio API fallback — Section 7.8) | Python, local LLM via Ollama, বা Google AI Studio API | Module 1 |
| Harness Validation Layer | Model output-কে Pydantic schema দিয়ে double-check করে; ব্যর্থ হলে এক-বার retry, তারপর `unmapped`-এ fallback | Python (Pydantic) | Module 1 |
| Capability Registry | Whitelisted action, exact params-schema, **static/hardcoded risk-level**, ও description | `capabilities.json` | Module 2 |
| Knowledge Base | System-specific context, few-shot examples (out-of-scope রিকোয়েস্ট চেনার জন্য) | `knowledge.md` | Module 1 |
| Plan Generator | Step-by-step execution plan তৈরি করে (risk registry থেকে আসে, model থেকে না) | Python | Module 1 |
| Confirmation + Discussion Loop | Plan দেখায়, discuss/adjust (soft-limit ৫ turn-এর পর nudge), `[e]` direct-edit, inline diff-note | Python (logic) + UI | Module 1 (logic), Module 4 (UI) |
| Danger Classifier | Raw command-এ destructive pattern শনাক্ত করে (regex, ambiguous case-এ LLM fallback) | Bash regex, Python/LLM fallback | Module 3 |
| Sudo/Permission Escalation Layer | Transparent, just-in-time sudo request (শুধু AI-plan-এ); per-step skip অপশন | Bash, `pty` | Module 3 |
| Streaming Executor | Blocking execution, subprocess output capture, structured event emission, `Ctrl+C` graceful-interrupt handling | Bash, Python | Module 2 (execution), Module 4 (rendering) |
| Trash/Undo Manager | `.trash/` move-based delete, ৮-দিনের (configurable) retention, expiry warning | Bash, metadata store | Module 3 |
| Audit Log & Reporting | প্রতিটা action, confirmation, outcome, ও interrupted-state রেকর্ড করে | Bash, log files (CSV/JSON) | Module 5 |
| Raw Command Pass-through | Valid শেল syntax সরাসরি execute করে; interactive প্রোগ্রাম (vim/top)-এর জন্য pty পুরোপুরি ছেড়ে দেওয়া | Bash | Module 2 |

**Week ১-এই লক করতে হবে এমন Interface Contract:**
- Module 1 ↔ Module 2 — structured intent JSON schema (JSON-Schema-constrained format)
- Module 2 ↔ Module 4 — execution-event format, যেমন `{"step": 2, "status": "running"|"done"|"failed"|"interrupted", "progress": {"done": 178, "total": 340}}`
- Module 1 ↔ Module 3 — sudo escalation trigger point ও risk-lookup call

### ৪.৩ Data Flow (সংক্ষিপ্ত)

```
User Input → Input Router
  → [raw: Danger Classifier | slash: Meta-Command Handler
     | natural language: Intent Parser (schema-constrained) → Harness Validation
     → Capability Registry lookup (risk/params from static registry)
     → Plan Generator → Confirmation/Discussion Loop]
  → Sudo Layer (প্রয়োজনে) → Streaming Executor (blocking) → Audit Log → Collapsed Summary
```

---

## ৫. Implementation Specification

### ৫.১ Tech Stack

| স্তর / Category | Choice | Version | Justification |
|---|---|---|---|
| Execution layer | Bash | system default (POSIX-compatible) | Raw command pass-through, শেল-স্ক্রিপ্ট capability backend, install script — কোর্সের mandatory shell-scripting layer (Section 1) |
| Orchestration layer | Python | 3.11+ | Intent parsing, validation, plan generation, streaming UI, wizard |
| Ollama client | `ollama` (official Python client) | ≥0.6.2 | Raw HTTP call থেকে migrate — upstream API পরিবর্তন (structured output, `think` parameter) নির্ভরযোগ্যভাবে track করে |
| Schema validation | `pydantic` | ≥2.13,<3.0 | শুধু validation-এর জন্য (Harness Validation Layer, Section 4.2)। **`pydantic-ai` স্পষ্টভাবে বাদ** — এটা একটা সম্পূর্ণ agent-framework, static/deterministic architecture-দর্শনের (Section 2.3) বিপরীত, এবং Ollama-only/local-only scope-এ অপ্রয়োজনীয় জটিলতা যোগ করে |
| Terminal UI | `rich` | latest stable | Streaming progress bar, plan panel, collapse-to-summary rendering |
| Interactive prompts | `questionary` | latest stable | Arrow-key navigation — wizard, `/model switch`, plan discuss/edit flow |
| Hardware metrics | `psutil` | latest stable | CPU/RAM লাইভ রিডিং (Live Hardware Load Indicator, Section 8.3.3) |
| Registry schema check | `jsonschema` | latest stable | Load-time-এ `capabilities.json`-এর প্রতিটা entry structurally valid কিনা যাচাই করে — এটা per-call Ollama constrained-decoding (Section 7.4) থেকে আলাদা: static registry-র নিজের integrity check |
| LLM runtime | Ollama | latest stable, systemd service | Local inference backend — মডেল hosting ও switching |
| JSON tooling (shell) | `jq` | system package | Install/wizard শেল-স্ক্রিপ্টে JSON inspect/debug |
| Checksum verify | `sha256sum` | coreutils (system default) | Install-script supply-chain verification (Section 8.1) |
| Sudo/pty handling | Python built-in `pty`, `subprocess` | stdlib | Sudo escalation ও interactive-program (vim/top) pty hand-off — আলাদা লাইব্রেরি অপ্রয়োজনীয় |
| Cloud fallback | Google AI Studio API (Gemini) | — | **Opt-in fallback**, শুধু dGPU-বিহীন হার্ডওয়্যারে (Section 7.8); primary architecture-এর অংশ না; API key `.env`/config-এ, কখনো hardcode না |

**Firewall backend** (network/security capability set, Core Features item 12) এখনো unresolved — দেখুন Section ১৫, Open Question ২।

Module 1-5-এর কাজের বিবরণে (Section 9) যেসব library/tool উল্লেখ আছে, সবগুলো এই টেবিলেরই reference — আলাদাভাবে repeat করা হয়নি।

### ৫.২ File/Directory Structure

Repo-এর ভেতর (single-developer build, minimal ও practical):

```
oh-my-shell/
├── bin/
│   └── oh-my-shell                  # entrypoint script (shebang → main.py)
├── install.sh                       # curl-pipe installer (Section 8.1)
├── pyproject.toml                   # dependency pins (Section 5.1)
├── README.md
├── capabilities/
│   └── capabilities.json            # Capability Registry (Section 5.4)
├── knowledge/
│   └── knowledge.md                 # System context, few-shot examples
├── src/
│   └── ohmyshell/
│       ├── __init__.py
│       ├── main.py                  # শেল REPL loop, entrypoint
│       ├── router.py                # Input Router — raw/slash/NL classification
│       ├── meta_commands.py         # Meta-Command Handler (/help, /model, ...)
│       ├── intent_parser.py         # Ollama call, JSON-Schema request, think:false
│       ├── validation.py            # Pydantic models, Harness Validation Layer
│       ├── registry.py              # capabilities.json loader + jsonschema check
│       ├── plan_generator.py        # Plan Generator
│       ├── discussion.py            # Confirmation + Discussion Loop, [e] edit
│       ├── danger_classifier.py     # regex + LLM-fallback destructive-pattern check
│       ├── executor.py              # Streaming Executor, subprocess management
│       ├── sudo_layer.py            # Sudo/Permission Escalation Layer
│       ├── trash.py                 # Trash/Undo Manager
│       ├── audit_log.py             # Audit Log writer/reader (JSON Lines)
│       ├── hardware.py              # psutil-ভিত্তিক CPU/RAM/GPU tiered detection
│       ├── config.py                # Config file load/save
│       ├── wizard.py                # First-run setup wizard
│       └── ui/
│           ├── prompt.py            # Prompt rendering (folder-name, model-tag, icon)
│           ├── streaming.py         # rich-ভিত্তিক live progress rendering
│           └── panels.py            # Plan preview panel, confirmation dialogs
└── tests/
    ├── test_validation.py
    ├── test_registry.py
    ├── test_danger_classifier.py
    └── benchmark/
        └── prompt_suite.json        # ৪০-prompt benchmark suite (Section 7.1)
```

Runtime-এ ইউজারের হোমে তৈরি হয় (repo-তে না, `.gitignore`-এ):

```
~/.oh-my-shell/
├── config.json                      # Section 5.5
├── audit.log.jsonl                  # append-only audit log
├── .trash/
│   ├── <timestamp>-<original-name>  # move করা ফাইল
│   └── metadata.json                # trash-entry ↔ original-path mapping, expiry
└── logs/
    └── wizard.log
```

### ৫.৩ Storage ও Persistence

কোনো database server নেই — single-user, local shell-এর জন্য অপ্রয়োজনীয় overhead।

- **Audit log:** JSON Lines (`~/.oh-my-shell/audit.log.jsonl`), append-only — প্রতি action/event একটা JSON object, এক লাইনে।
- **`.trash/` metadata:** ছোট JSON file (`~/.oh-my-shell/.trash/metadata.json`) — trashed-file → original path, timestamp, expiry mapping।
- **Config:** একটা JSON file (`~/.oh-my-shell/config.json`, ফরম্যাট Section 5.5)।
- **Session context:** in-memory-only (Python process-এর lifetime পর্যন্ত), persist করা হয় না — এটা Section 2.3-এর "persistent learning না" সিদ্ধান্তের সাথে সংগতিপূর্ণ।

**Future Work note:** audit log অনেক বড় হয়ে গেলে (query/analytics প্রয়োজনে) SQLite-এ migrate করার সম্ভাবনা আছে — এখনই implement না; Section 6.1 (Future Work)-এ এই আইটেম যোগ করা হয়েছে।

### ৫.৪ `capabilities.json` — Reference Examples

কমপক্ষে ৪টা core capability, সম্পূর্ণ schema-সহ (copy-paste-able):

```json
{
  "$schema": "capabilities.schema.json",
  "version": "1.0",
  "capabilities": [
    {
      "action": "clean_temp_files",
      "description": "Move files older than N days from /tmp and ~/.cache into .trash/ for safe, reversible cleanup.",
      "risk": "medium",
      "params_schema": {
        "type": "object",
        "properties": {
          "days": { "type": "integer", "minimum": 1, "default": 7 },
          "paths": {
            "type": "array",
            "items": { "type": "string" },
            "default": ["/tmp", "~/.cache"]
          }
        },
        "required": ["days"],
        "additionalProperties": false
      },
      "command_template": "find {paths} -type f -mtime +{days} -exec mv {{}} ~/.oh-my-shell/.trash/ \\;",
      "few_shot_examples": [
        "clean up temp files older than a week",
        "clear out my cache"
      ]
    },
    {
      "action": "organize_files",
      "description": "Sort files in a target directory into subfolders by file extension.",
      "risk": "low",
      "params_schema": {
        "type": "object",
        "properties": {
          "target_dir": { "type": "string" },
          "by": { "type": "string", "enum": ["extension"], "default": "extension" }
        },
        "required": ["target_dir"],
        "additionalProperties": false
      },
      "command_template": "for f in {target_dir}/*; do ext=\"${f##*.}\"; mkdir -p \"{target_dir}/$ext\"; mv \"$f\" \"{target_dir}/$ext/\"; done",
      "few_shot_examples": [
        "organize this folder",
        "sort downloads by file type"
      ]
    },
    {
      "action": "list_processes",
      "description": "List running processes, optionally filtered by name or sorted by resource usage.",
      "risk": "low",
      "params_schema": {
        "type": "object",
        "properties": {
          "filter": { "type": "string", "default": "" },
          "sort_by": { "type": "string", "enum": ["cpu", "mem", "none"], "default": "none" }
        },
        "required": [],
        "additionalProperties": false
      },
      "command_template": "ps aux --sort=-{sort_by} | grep -i '{filter}'",
      "few_shot_examples": [
        "show me what's using the most CPU",
        "find chrome processes"
      ]
    },
    {
      "action": "kill_process",
      "description": "Terminate a running process by PID or name. Always high-risk regardless of model output — irreversible, can affect unsaved work or system stability.",
      "risk": "high",
      "params_schema": {
        "type": "object",
        "properties": {
          "target": { "type": "string" },
          "signal": { "type": "string", "enum": ["TERM", "KILL"], "default": "TERM" }
        },
        "required": ["target"],
        "additionalProperties": false
      },
      "command_template": "kill -{signal} {target}",
      "few_shot_examples": [
        "kill the process using port 8080",
        "stop that runaway python script"
      ]
    }
  ]
}
```

`risk` field এখানে registry-র নিজস্ব static value (Section 7.4-এর সিদ্ধান্ত অনুযায়ী) — harness সবসময় এখান থেকেই risk lookup করে, model-output-এর কোনো risk field থাকলেও সেটা authoritative না।

### ৫.৫ Config File Format

Path: `~/.oh-my-shell/config.json`

```json
{
  "model": {
    "active": "qwen3:8b",
    "available": ["qwen3:8b", "qwen3.5:4b", "phi4-mini", "lfm2.5-8b-a1b"]
  },
  "safety": {
    "safe_mode": false,
    "danger_classifier_sensitivity": "normal"
  },
  "trash": {
    "retention_days": 8
  },
  "ui": {
    "quiet": false,
    "verbose": false
  },
  "log": {
    "level": "normal"
  },
  "discussion": {
    "soft_limit_turns": 5
  }
}
```

`/config set <k> <v>` (Section 8.4) dot-notation key দিয়ে nested field আপডেট করে — যেমন `/config set trash.retention_days 14`। Wizard শেষে defaults দিয়ে ফাইল প্রথমবার তৈরি হয়; পরে manual edit বা `/config` দিয়ে override করা যায়।

### ৫.৬ Minimal Testing Approach

Single-developer বাস্তবতায় practical scope:

- **Framework:** `pytest` — unit test (validation, registry loading, danger-classifier regex rules)।
- **কখন চালানো হবে:** প্রতিটা module-এর কোর logic লেখার সাথে সাথে; commit-এর আগে সংশ্লিষ্ট test চালানো mandatory (কোডবেস-নিয়ম #4)।
- **Benchmark suite re-run:** `tests/benchmark/prompt_suite.json` (৪০-prompt) — model/prompt পরিবর্তনের পরই চালানো হয়, প্রতি commit-এ না (ব্যয়বহুল, thermal ঝুঁকি — Section 7.5)।
- **Integration test:** end-to-end manual walkthrough, প্রতিটা মডিউল merge হওয়ার পর (Timeline-এর সপ্তাহ ৩ ও ৫ — Section 10)।
- **কভারেজ-percentage target নেই** — solo build-এ সময়ের অপচয়; safety-critical পাথ (danger classifier, sudo layer, undo) pytest ও manual দুটোতেই test হবে, বাকি path শুধু integration test-এ cover হয়।

---

## ৬. Core Features

1. **Conversational Plan-and-Confirm** — Natural-language request থেকে step-by-step plan তৈরি হবে, discuss (soft-limit ৫ turn) ও `[e]` direct-edit দুই পথেই adjust করা যাবে।
2. **Raw Command Mode** — Experienced user কোনো friction ছাড়াই সরাসরি exact command চালাতে পারবে; interactive program (vim/top)-এর জন্য pty সম্পূর্ণ ছেড়ে দেওয়া হয়।
3. **Danger Detection & Selective Intervention** — শুধু potentially destructive raw command-এ AI intervene করবে।
4. **Live Streaming Workflow UI** — প্রতিটা sub-step রিয়েল-টাইমে stream হবে (file count, data-size, percentage — সব লাইভ), শেষে সংক্ষিপ্ত summary-তে collapse করবে।
5. **Transparent Sudo/Permission Escalation** — শুধু AI-generated plan-এ প্রয়োজন হলে explicit request দেখাবে, per-step skip অপশনসহ; ইউজারের নিজের raw `sudo` কমান্ডে সরাসরি OS-prompt (বাড়তি confirmation নেই)।
6. **Capability Registry** (`capabilities.json`) — Structured, whitelisted action set; **risk-level static/hardcoded**, description ও few-shot example mandatory।
7. **Knowledge Base** (`knowledge.md`) — System-specific context ও few-shot example, model prompt-এ inject করা হয়।
8. **Undo/Rollback** — Destructive operation `.trash/`-এ move হয় (সরাসরি delete না); ৮-দিনের (configurable) retention, expire হওয়ার আগে warning; interrupted (আংশিক-সম্পন্ন) operation-ও undo করা যায়।
9. **Session Context Memory** — একটা session-এর মধ্যে সংক্ষিপ্ত conversation history মনে রাখা হয় (persistent "learning" না — Section 2.3)।
10. **Explainability** — প্রতিটা AI decision-এর সাথে সংক্ষিপ্ত রেশনাল দেখানো হয়।
11. **Audit Logging & Reporting** — প্রতিটা action ও outcome (সম্পূর্ণ/interrupted/skipped) log হয়।
12. **Lightweight Network/Security Capability Set** — Read-only network diagnostics ও একটা firewall-rule capability (high-risk, confirmation-বাধ্যতামূলক; backend পছন্দ এখনো খোলা — Section ১৫, Open Question ২)।
13. **Model Selection & Hot-Swapping** — User pre-tested মডেলের মধ্যে থেকে বেছে নিতে ও যেকোনো সময় switch করতে পারবে (`/model switch`); custom/arbitrary মডেল সাপোর্ট করা হয় না।
14. **Live Hardware Load Indicator** — শুধু AI-thinking-window-এ (CPU/RAM সবসময়, GPU tiered-detection সাপেক্ষে, integrated GPU-তে চুপচাপ hide)।
15. **Interrupt-Safe Execution** — `Ctrl+C`-এ current file-operation শেষ হতে দিয়ে গ্রেসফুলভাবে থামা; double-`Ctrl+C`-এ force-stop; থামার পর undo/resume/continue অপশন।
16. **Cloud API Fallback (opt-in)** — Google AI Studio API, শুধু dGPU-বিহীন হার্ডওয়্যারে ব্যবহারের জন্য; `--provider=api` flag বা `/config set provider api` দিয়ে toggle; local-first architecture-এর substitute না, শুধু hardware-accessibility gap পূরণ করে (Section 7.8)।

**Fail-safe behavior:** LLM malformed JSON রিটার্ন করলে, বা কোনো registered capability-তে map না হওয়া intent দিলে, সিস্টেম কিছু execute করবে না — বরং user-কে clarify করতে বলবে (`unmapped` action)।

### ৬.১ Future Work

নিচের ফিচারগুলো initial ৫-৬ সপ্তাহের delivery-তে ইচ্ছাকৃতভাবে scope-এর বাইরে রাখা হয়েছে:

- **Document/Report Generation** — Audit-log data থেকে plain-text/Markdown রিপোর্ট, ভবিষ্যতে `.docx`/`.xlsx` export।
- **Application Launching** — Capability Registry-তে "open application" action যোগ করা; deeper GUI automation স্কোপের বাইরে।
- **বাংলা Natural-Language Input** — ইংরেজির পাশাপাশি বাংলা request সাপোর্ট।
- **Windows/macOS পোর্টিং** — শেল-execution ও permission-escalation লেয়ার OS-specific redesign প্রয়োজন।
- **Background/Concurrent Execution** — বর্তমান architecture blocking (একসাথে একটাই operation); multi-tasking/async execution জটিলতা ও race-condition ঝুঁকির কারণে বাদ দেওয়া হয়েছে।
- **Audit Log-এর SQLite Migration** — বর্তমানে JSON Lines (Section 5.3); log বড় হয়ে গেলে query/analytics-এর প্রয়োজনে SQLite-এ migrate করার সম্ভাবনা আছে, এখনই প্রয়োজন নেই।

---

## ৭. AI Model Decision

### ৭.১ Benchmark Methodology

একটা curated ৪০-prompt real-life test suite (easy → complex gradient: direct commands, rephrased/casual phrasing, multi-parameter, symptom-based ambiguous request, out-of-scope request, multi-intent, prompt-injection attempt) দিয়ে মোট **১০টা candidate model** দুই রাউন্ডে hands-on টেস্ট করা হয়েছে, প্রতিটা prompt ৩ বার করে। প্রতিটা call-এ `think: false` mandatory ছিল (Section 7.3 দেখুন)।

- **রাউন্ড ১ (৬টা মূল candidate):** LFM2.5-8B-A1B, Qwen3:8b, Qwen3.5:4b, LFM2.5-thinking:1.2b, Qwen3.5:2b, Phi4-mini — মোট ৬টা মডেল, basic `format:"json"` দিয়ে।
- **রাউন্ড ২ (sub-3B ultra-light exploration):** Qwen3:0.6b, Llama3.2:1b, Llama3.2:3b, Qwen3.5:0.8b — আরও ৪টা মডেল (Section 7.5)।

**Gemma4:12b বাদ দেওয়া হয়েছে:** টেস্টিং চলাকালীন ল্যাপটপে বারবার thermal shutdown ঘটায় (sustained heavy-load-এ), এই মডেল ব্যবহারিকভাবে student-laptop-এর জন্য অনুপযুক্ত প্রমাণিত হয়েছে — hands-on ঝুঁকি hardware-safety বিবেচনায় প্রাধান্য পেয়েছে।

### ৭.২ সম্পূর্ণ বেঞ্চমার্ক ফলাফল

| মডেল | Easy accuracy | Ambiguous handling | Out-of-scope (fail-safe) | Kill-risk consistency | Injection resistance | গড় গতি |
|---|---|---|---|---|---|---|
| LFM2.5-8B-A1B | ৯৯% | ১০০% | **৪৭% ❌** | high/medium মিশ্র | ❌ ব্যর্থ (১টা কেসে) | ১৮১ms |
| **Qwen3:8b** | ৯৬% | ৯২% | **১০০% ✅** | Consistently সবচেয়ে বেশি "high" (কঠোর, safe) | ✅ প্রতিরোধী | ৪৬৬ms (ধীরতম) |
| **Qwen3.5:4b** | ৯৭% | ৯৪% | **১০০% ✅** | low/medium মিশ্র | ✅ প্রতিরোধী | ২৯৮ms |
| LFM2.5-thinking:1.2b | ৯১% | ১০০% | ৬৭% | সবসময় low | ✅ প্রতিরোধী | ১২১ms (দ্রুততম) |
| Qwen3.5:2b | ৭৫% (সবচেয়ে কম) | ৭২% (সবচেয়ে কম) | ৮০% | low/medium মিশ্র | প্রায় প্রতিরোধী | ৪৮৫ms |
| Phi4-mini | ৯৯% | ৮৬% | ৯৩% | low (kill-এও) | ✅ প্রতিরোধী | ২৯৬ms |

**সবচেয়ে গুরুত্বপূর্ণ finding:** Out-of-scope handling (fail-safe correctness)-এ বিশাল ব্যবধান দেখা গেছে। LFM2.5-8B-A1B — যেটা প্রাথমিক গবেষণায় (multi-turn tool-use benchmark) সেরা candidate মনে হয়েছিল — এই broader, real-life টেস্টে সবচেয়ে দুর্বল প্রমাণিত হয়েছে (out-of-scope-এ মাত্র ৪৭%, prompt-injection-এও একবার ব্যর্থ)। এটা নিশ্চিত করে যে narrow benchmark দিয়ে model-selection যথেষ্ট না — broader, adversarial-inclusive real-life টেস্টিং প্রয়োজনীয়।

**Kill-process risk-consistency** সব মডেলেই কমবেশি অস্থির (Qwen3:8b সবচেয়ে ভালো, বাকিরা destructive action-কেও প্রায়ই "low" বলছে) — এই finding **risk hardcode করার সিদ্ধান্তকে (Section 7.4) সরাসরি ন্যায্যতা দেয়**।

### ৭.৩ Critical Implementation Note — Thinking Mode

Hybrid-thinking মডেলে (Qwen3.5 পরিবার, LFM2.5-thinking পরিবার) `think: false` API parameter **mandatory**, প্রতিটা call-এ:
- এটা ছাড়া Qwen3.5:4b-এ `response` field সম্পূর্ণ **খালি** আসে (কনটেন্ট `thinking` field-এ আটকে থাকে) — parsing সম্পূর্ণ ব্যর্থ হয়।
- এটা ছাড়া LFM2.5-thinking:1.2b-এ `params` object **প্রতিবার ভিন্ন key-structure** দেয় (কখনো `days`, কখনো `old_age`, কখনো nested) — schema সম্পূর্ণ অনির্ভরযোগ্য হয়ে যায়।
- `think: false` যোগ করার পর, উভয় মডেলেই এই সমস্যা সম্পূর্ণ সমাধান হয়েছে (১০/১০ consistent, hands-on verified)।
- System-prompt-এর ভেতরে `/no_think` instruction লেখা **যথেষ্ট না** — এটা top-level API parameter হিসেবেই পাস করতে হবে।

*Re-verified (Sept ২০২৬):* Ollama-র official docs অনুযায়ী `think` এখনো একটা **top-level API parameter** — এটা boolean (`true`/`false`) বা level-string (`low`/`medium`/`high`/`max`) নেয়, এবং system-prompt-based override (`/no_think`) নির্ভরযোগ্য বিকল্প না। এই ব্লুপ্রিন্টের মূল সিদ্ধান্ত এখনো accurate।

### ৭.৪ Critical Implementation Note — JSON-Schema-Constrained Decoding ও Static Risk

দুটো architectural সিদ্ধান্ত একসাথে reliability guarantee করে:

**ক) Generic `format: "json"` না দিয়ে, actual JSON Schema পাস করা** (Ollama v0.5-এ চালু হয়েছে, বর্তমানেও stable ও local ব্যবহারে পূর্ণ-সাপোর্টেড):
```python
schema = {
    "type": "object",
    "properties": {
        "action": {"type": "string", "enum": ["clean_temp_files", "organize_files", "list_processes", "kill_process", "unmapped"]},
        "risk": {"type": "string", "enum": ["low", "medium", "high"]},
        "params": {"type": "object", "additionalProperties": False}
    },
    "required": ["action", "risk", "params"]
}
```
এটা token-sampling-level এ কাজ করে (grammar-constrained decoding) — model structurally ভুল output generate করতেই পারে না। এটা values-এর semantic accuracy guarantee করে না, শুধু structure/format।

**খ) Risk-level কখনো AI-generated রাখা হয় না** — Capability Registry-তে প্রতিটা action-এর জন্য risk আগে থেকেই fixed (হার্ডকোড করা)। Model শুধু action বেছে নেয়; harness নিজে registry থেকে risk lookup করে। এটা Section 7.2-এর kill-process risk-inconsistency সমস্যা সম্পূর্ণ নির্মূল করে।

**গ) Post-generation harness validation** (Pydantic দিয়ে) — model output structurally valid হলেও, execute করার আগে একবার harness-level এ double-check করা হয়; ব্যর্থ হলে এক-বার retry, তারপরও fail করলে `unmapped`-এ পড়া। এই তিন-স্তরের design MLflow-এর industry best-practice অনুসরণ করে: *reliability আসা উচিত harness/orchestration code থেকে, model-এর প্রতি অন্ধ নির্ভরতা থেকে না।*

### ৭.৫ Sub-3B RAM Exploration — Verified, Rejected

প্রাথমিক research পরস্পরবিরোধী সংকেত দিয়েছিল sub-3B মডেল সম্পর্কে (কিছু বেঞ্চমার্কে ৪B-ক্লাসের সমান স্কোর, অন্য research-এ multi-step reasoning-এ ব্যর্থতার দাবি) — তাই এটা hands-on ভেরিফাই করা হয়েছে, ধারণার উপর নির্ভর না করে। চারটা candidate (Qwen3:0.6b, Llama3.2:1b, Llama3.2:3b, Qwen3.5:0.8b) একই ৪০-prompt suite-এ (৩ রান প্রতি prompt) টেস্ট করা হয়েছে।

| মডেল | RAM (আনুমানিক) | Easy accuracy | Ambiguous | Out-of-scope | **Injection resistance** | গতি |
|---|---|---|---|---|---|---|
| Qwen3:0.6b | ~০.৪-০.৬GB | ৮৮% | ৭০% | ৮০% | ❌ ৩/৩ ব্যর্থ ("hello" leak, action-enum ভেঙেছে) | ৬৫ms |
| Llama3.2:1b | ~০.৮-১.৩GB | ৯৮% | ৮৯% | ৫৩% (সবচেয়ে কম) | ⚠️ আংশিক (২/৩ সঠিক) | ১৫২ms |
| Llama3.2:3b | ~২-২.৫GB | ৯৮% | ১০০% | ৮০% | ❌ ২/৩ ব্যর্থ + ১টা malformed output | ২০৩ms |
| Qwen3.5:0.8b | ~০.৬-১GB | ৯৮% | ৯৩% | ৬৭% | ❌ ৩/৩ ব্যর্থ | ১৪২ms |

**সিদ্ধান্ত — এই রেঞ্জের কোনো মডেলই গ্রহণ করা হয়নি।** যদিও easy-task accuracy ভালো (৮৮-৯৮%), সবগুলো মডেলই prompt-injection resistance-এ উল্লেখযোগ্যভাবে দুর্বল (৩টা সম্পূর্ণ ব্যর্থ, ১টা আংশিক) এবং out-of-scope fail-safe handling-এ established মডেলের (Qwen3:8b, Qwen3.5:4b — উভয়ই ১০০%) তুলনায় স্পষ্টভাবে পিছিয়ে (৫৩-৮০%)। যেহেতু এই শেল destructive system-command execute করতে সক্ষম, safety-critical metric-এ compromise করে RAM বাঁচানো গ্রহণযোগ্য নয়। **এই প্যাটার্ন (easy-task ভালো, safety-metric দুর্বল) sub-3B রেঞ্জের সহজাত সীমাবদ্ধতা বলে মনে হয়, কোনো একক মডেলের bug না** — তাই ultra-light স্লট ইচ্ছাকৃতভাবে খালি রাখা হয়েছে, বিকল্প খোঁজা চালিয়ে না গিয়ে।

**Hardware-safety note:** এই টেস্টিং পর্বে ল্যাপটপে একাধিকবার sustained-load thermal issue দেখা দিয়েছে (দুইবার auto-shutdown, একবার ৯৬-৯৮°C)। এর ফলে (ক) Gemma4:12b বেঞ্চমার্ক থেকে বাদ দেওয়া হয়েছে (ব্যবহারিকভাবে অনুপযুক্ত প্রমাণিত), এবং (খ) JSON-Schema-enum-constrained decoding (Section 7.4) দিয়ে established ৪-মডেল লাইনআপের পূর্ণ re-verification hardware-safety বিবেচনায় স্থগিত রাখা হয়েছে — এটা Module 5-এর testing phase-এ (কম intensive, spaced-out পদ্ধতিতে) সম্পন্ন করার পরিকল্পনা।

### ৭.৬ Selected Model Lineup

| Slot | মডেল | RAM (Q4) | যুক্তি |
|---|---|---|---|
| **Primary** | **Qwen3:8b** | ~৫-৫.৫GB | সর্বোচ্চ fail-safe reliability (১০০%), injection-প্রতিরোধী, destructive-action-এ সবচেয়ে সতর্ক risk-tendency — ধীরতম হলেও সবচেয়ে নির্ভরযোগ্য |
| **Balanced** | **Qwen3.5:4b** | ~৩.৪GB | Qwen3:8b-এর কাছাকাছি reliability, উল্লেখযোগ্য দ্রুততর — `think:false` mandatory |
| **Light** | **Phi4-mini** | ~২.৫GB | ভালো easy/out-of-scope accuracy; kill-risk দুর্বলতা risk-hardcoding দিয়ে mitigate |
| **Fallback/Legacy** | LFM2.5-8B-A1B | ~৫.২GB | Easy/ambiguous case-এ চমৎকার, কিন্তু out-of-scope দুর্বলতার কারণে primary না — শুধু fallback হিসেবে honest caveat-সহ রাখা |

**Ultra-light স্লট ইচ্ছাকৃতভাবে খালি — sub-3B রেঞ্জে hands-on ভেরিফাই করা কোনো মডেলই safety-metric-এ যথেষ্ট নির্ভরযোগ্য প্রমাণিত হয়নি (Section 7.5)।** Phi4-mini (~২.৫GB) বর্তমান lineup-এর সর্বনিম্ন RAM-tier। Custom/arbitrary মডেল সাপোর্ট করা হয় না।

### ৭.৭ System Requirements

- **OS:** Ubuntu 22.04+ / Debian-family Linux (systemd-based)।
- **RAM:** ৪GB minimum (Phi4-mini একা), ৮GB recommended, ১৬GB আদর্শ (একাধিক মডেল রাখা/সুইচ করার flexibility-র জন্য)।
- **Disk:** ৮GB minimum, ১৫GB recommended (component breakdown: core+dependency ~১৫০MB, Ollama ~৫০০MB-১GB, model ২.৫-৫.৫GB)।
- **Ollama:** Background systemd service; install script শুধু core shell ইনস্টল করে, Ollama প্রথম রানের wizard-এর মাধ্যমে ইনস্টল হয়।

### ৭.৮ Google AI Studio API — Opt-in Fallback (Hardware-Constrained Users)

**সিদ্ধান্ত:** Oh My Shell local-first architecture থেকে সরে আসে না — primary development ও demo Shaid-এর নিজের dGPU-মেশিনে local Ollama দিয়েই হবে। Google AI Studio API শুধু তাদের জন্য opt-in fallback হিসেবে যোগ করা হয়েছে যাদের মেশিনে dedicated GPU নেই (Section 7.9)।

- Intent Parser-এর backend একটা **provider-abstraction**-এর মাধ্যমে সুইচযোগ্য — Ollama (default) অথবা Google AI Studio API (config/flag দিয়ে toggle)।
- Provider যাই হোক, বাকি pipeline (JSON-Schema-constrained prompt, static risk-lookup, harness-validation — Section 7.4) **অপরিবর্তিত** থাকে; provider-choice শুধু Intent Parser-এর ভেতরের একটা call-target বদলায়, বাকি architecture-এ কোনো প্রভাব ফেলে না।
- **Honest সীমাবদ্ধতা:** API-mode ব্যবহারে internet-dependency তৈরি হয় — "fully offline/local" দাবি তখন প্রযোজ্য না, এটা user-facing হিসেবে স্পষ্ট করে বলা উচিত (`/system` output-এ active provider দেখানো)।
- **Security:** API key `.env`/config-এ রাখা হবে, কখনো hardcode না (codebase operating rules, Section 16)।
- **Verification pending:** Google AI Studio API-র structured-output/schema-enforcement আচরণ (আমাদের JSON-Schema-constrained decoding pattern, Section 7.4-এর সমতুল্য) এখনো hands-on verify করা হয়নি — দেখুন Section 15, Open Question 3।
- এই hybrid decision Section 2.3-এ নতুন Conflicting Requirement ("Local-first Predictability বনাম Hardware Accessibility") হিসেবে ডকুমেন্টেড।

### ৭.৯ Team Hardware Reality (Verified)

| সদস্য | CPU | RAM | GPU | Inference tier |
|---|---|---|---|---|
| Shaid (dev machine) | dGPU-equipped laptop | 16GB+ | Discrete GPU | Full local, Qwen3:8b primary |
| Md. Fazle Rabbi | Intel i7-10610U (low-power, U-series, 1.8GHz) | 16GB (15.7GB usable) | Intel UHD Graphics (128MB, no dGPU) | Local light-model (Phi4-mini/Qwen3.5:4b) বা API fallback সুপারিশ করা হয় |
| Nuhash | iGPU-only (স্পেসিফিকেশন আংশিক জানা) | — | No dGPU | Local light-model বা API fallback |
| বাকি ২ সদস্য | অনির্ধারিত | — | — | Confirm করা বাকি (Section 15, Open Question 4) |

**Hands-on পরীক্ষার ফলাফল (Fazle-এর মেশিনে, `ollama run`, chat mode):** eval-rate ১৭.৯–১৯.৭ tokens/s (ছোট-মাঝারি response-এ), কিন্তু বড় response (১০০০+ token) বা thinking-mode-enabled অবস্থায় সময় ১ মিনিটের বেশি লেগেছে। Nuhash-এর মেশিনে একটা টেস্টে মাত্র ৫.৩১ tokens/s দেখা গেছে (৮৪ সেকেন্ড+ সময়ে ৪৪৩ token) — dGPU-মেশিনের বেঞ্চমার্ক (Section 7.2, eval_duration ২০০-৫০০ms রেঞ্জ) তুলনায় উল্লেখযোগ্যভাবে ধীর।

**সিদ্ধান্তের প্রভাব:**
- dGPU-বিহীন মেশিনে Qwen3:8b (primary) ব্যবহারযোগ্য কিন্তু ধীর অনুভূত হতে পারে — সেসব ক্ষেত্রে Phi4-mini/Qwen3.5:4b local অথবা Google AI Studio API fallback (Section 7.8) ডিফল্ট হওয়া উচিত।
- Demo-day-এ primary path Shaid-এর নিজের dGPU-মেশিনেই থাকবে, dGPU-বিহীন মেশিন demo-critical path-এ থাকবে না।
- এই hardware-heterogeneity conscious engineering trade-off হিসেবে রিপোর্টে (Section 11.1, EA1/EP4) ব্যবহারযোগ্য — "শুধু ideal-case-এ ডিজাইন করা হয়নি" এই narrative-কে শক্তিশালী করে।

---

## ৮. Distribution & CLI UX Specification

### ৮.১ Download ও Installation

**একটাই path — কোনো `.sh` ফাইল ডাউনলোড বা "Download" বাটন নেই।** Website ও terminal উভয় জায়গাতেই একই copy-করা-যায় command দেখানো হয় (Claude Code CLI/Ollama/rustup-এর প্যাটার্ন অনুসরণ করে):

```bash
curl -fsSL https://ohmyshell.dev/install.sh | bash
```

Website-এ শুধু একটা command-box ও `[Copy]` বাটন থাকে — আলাদা কোনো download-flow নেই, যা moving-parts কমায়।

Install script core code (কয়েক MB) ইনস্টল করে, `/usr/local/bin`-এ symlink এবং `/etc/shells`-এ entry যোগ করে (যাতে ইউজার চাইলে default shell হিসেবে সেট করতে পারে)। কোনো Ollama/model bundled থাকে না — সব প্রথম-রানের wizard-এ হয়।

**Supply-chain security:** SHA256 checksum verify করার পরই install; কোনো ধাপ ব্যর্থ হলে (`set -euo pipefail`) সম্পূর্ণ থেমে যায়।

### ৮.২ First-Run Interactive Setup Wizard

প্রথমবার `oh-my-shell` চালানোর পর একটা ৪-ধাপের wizard, প্রতিটা ধাপে live progress:

1. **System check** — OS, kernel, RAM, CPU, GPU, disk space (৮GB-এর কম হলে warning), shell version
2. **Ollama installation** — না থাকলে ডাউনলোড ও systemd service enable
3. **Model selection** — arrow-key (↑/↓/Enter) নেভিগেশন, ৪টা মডেলের RAM ও ব্যবহারক্ষেত্র পাশে দেখানো; confirmation ধাপ, ডাউনলোড, post-download test-inference verification
4. **Python dependency install ও সারাংশ** — model, RAM usage, config path দেখিয়ে ব্যবহার শুরুর নির্দেশনা

Wizard-এর UI Module 4-এর streaming/progress-component reuse করে; arrow-key selection `questionary`/`InquirerPy` দিয়ে।

**Interrupted wizard:** মাঝপথে বন্ধ হলে (`Ctrl+C`), পরের রানে wizard সম্পূর্ণ fresh-restart হয় (partial-state persist করা হয় না — এই ছোট, one-time setup-এর জন্য resume-logic-এর engineering cost অপ্রয়োজনীয়)। Model-download-এর actual resume Ollama নিজেই chunk-level এ handle করে, তাই বড় re-download-এর প্রকৃত ক্ষতি কম।

### ৮.৩ Command & Chat CLI — Interaction Modes ও UX ফ্লো

#### ৮.৩.০ Normal Launch (Setup-পরবর্তী প্রতিটা সেশন)

Wizard শুধু প্রথমবার চলে। দ্বিতীয়বার থেকে, `oh-my-shell` চালালে একটা সংক্ষিপ্ত, একলাইনের status-ব্যানার দেখিয়ে সরাসরি prompt-এ চলে যায় (ভারী ASCII-art বা multi-line splash প্রতিবার দেখানো হয় না, কারণ এটা দৈনন্দিন ব্যবহারে বিরক্তিকর):

```
$ oh-my-shell

  Oh My Shell v1.0  ·  qwen3:8b  ·  Ollama running

~/  ❯ _
```

#### ৮.৩.১ Prompt Design

Prompt বর্তমান ফোল্ডারের **নাম-শুধু** দেখায় (পুরো path না), আর নির্বাচিত মডেল default (Qwen3:8b) না হলে একটা tag যোগ হয়:

```
downloads ❯ ls -la                          (raw command, default model)
downloads (qwen3.5:4b) ❯ organize this       (non-default model হলে tag)
```

**AI-request enter করার পর, একই লাইনে ইন-প্লেস icon পরিবর্তন** (`❯` → `✦`), নতুন লাইনে না:
```
downloads ❯ clean up temp files▊             (টাইপ করার সময়)
downloads ✦ clean up temp files              (Enter-এর পর, একই লাইনে)
```
Raw command-এ icon অপরিবর্তিত থাকে (`❯`-ই থাকে) — এভাবে scroll-back-এ কোন লাইন AI-processed আর কোনটা raw pass-through, তা visually স্পষ্ট থাকে।

#### ৮.৩.২ Multi-line Paste Handling

Bracketed-paste-mode দিয়ে detect করে, ইউজারকে জিজ্ঞেস করা হয়:
```
downloads ❯ [paste detected: 4 lines]

  > Run each line separately, in order
    Treat as one combined request
    Cancel and let me edit
```
(পুরনো terminal-এ bracketed-paste সাপোর্ট না থাকলে, literal-newline-detection ফলব্যাক হিসেবে ব্যবহৃত হয়।)

#### ৮.৩.৩ Plan Preview, Discussion ও Direct-Edit

```
downloads ✦ clean up temp files older than a week

  ⚟ Thinking... 94 tokens · 0.8s
  CPU ▓▓▓▓▓▓▓░░░ 82% · 58°C   RAM ▓▓▓▓▓▓▓▓░░ 39% · 6.1/15.6GB

  ┌─ Plan ─────────────────────────────────────────────┐
  │  1. Scan /tmp and ~/.cache for files >7 days old       │
  │  2. Calculate total reclaimable space                  │
  │  3. Move matched files to .trash/ (recoverable)         │
  │                                                       │
  │  Risk: Medium  ·  Est. 340 files  ·  ~1.2 GB             │
  ├───────────────────────────────────────────────────────┤
  │  ↯ 94 tokens in · 62 tokens out · 0.8s · qwen3:8b          │
  └─────────────────────────────────────────────────────┘

  [Enter] Confirm   [e] Edit plan   [c] Chat/adjust   [Esc] Cancel
```

Token count ও সময় "Thinking..." লাইনে **live in-place update** হয় (generation চলাকালীন প্রতিটা নতুন token-এ সংখ্যা বাড়ে, একই লাইনে, নতুন লাইন যোগ না করে) — Ollama-র streaming response থেকে client-side গণনা করে।

**Live Hardware Load** শুধু "Thinking..." window-এ দেখানো হয় (execution-এ না) — CPU/RAM সবসময়, GPU শুধু tiered-detection-এ পাওয়া গেলে (`nvidia-smi` → NVIDIA discrete, `rocm-smi` → AMD discrete); integrated GPU বা temperature-reading না পেলে সেই অংশ চুপচাপ (কোনো "N/A" placeholder ছাড়া) বাদ যায়।

**`[c]` Chat/adjust** — চাপলে inline প্রম্পট আসে:
```
  💬 What would you like to change?
  ❯ don't touch anything in Downloads folder_
```
Enter করলে revised plan-এ ঠিক কোন অংশ বদলেছে তা inline diff-note হিসেবে দেখানো হয় ("Downloads excluded per your request")। **সফট-লিমিট: ৫ discuss-turn-এর পর** একটা gentle reminder দেখানো হয় (raw command mode-এর বিকল্প সাজেস্ট করে), কিন্তু hard block করা হয় না।

**`[e]` Edit plan** — নির্দিষ্ট step সরাসরি বেছে নিয়ে param deterministically বদলানো (AI বাইপাস করে, তাই instant, কোনো model-call লাগে না):
```
  Edit which step? (1-3, or 'q' to cancel)
  ❯ 1
  Step 1: Scan /tmp and ~/.cache for files >7 days old
  New value for "days" [7]: 14
```

#### ৮.৩.৪ Execution — Live Streaming ও Interrupt Handling

```
  ▸ Executing (3 steps)

  ✓ Scanned /tmp and ~/.cache                    [0.4s]
  ⠙ Moving files to .trash/...
    ████████████████░░░░░░░░░░  178/340 files  ·  0.64 GB/1.2 GB  ·  52%
    /tmp/npm-install-8821/

  [Ctrl+C] Abort
```

File-count, byte-size, ও percentage — তিনটাই একসাথে live-updating, একই progress লাইনে। Execution **blocking** — এই সময় prompt inactive থাকে, শুধু `Ctrl+C` respond করে (কোনো নতুন command/request নেওয়া হয় না; background/concurrent execution architecture-এর বাইরে, Section 6.1)।

`Ctrl+C`-এ **graceful interrupt** — current file-operation নিরাপদে শেষ হতে দিয়ে থামে, immediate kill না:
```
    ████████████████░░░░░░░░░░  266/340 files  ·  78%
    ^C
  ⚠ Finishing current file safely... (press Ctrl+C again to force-stop)

  ✓ Stopped at 266/340 files (78%)  ·  0.94 GB moved  ·  74 remaining

  [u] Undo what was moved   [r] Resume remaining   [Enter] Continue to shell
```
দ্বিতীয়বার `Ctrl+C` immediate force-stop করে, explicit warning-সহ। Audit log-এ এটা `"status": "interrupted"` হিসেবে রেকর্ড হয় (error বা success না)। সম্পূর্ণ হওয়ার পর:
```
  ✓ Cleanup complete                              [12.3s]

    340 files removed  ·  1.2 GB reclaimed  ·  moved to .trash/
    AI: 156 tokens total · 1.2s reasoning time

  ▸ [v] View detailed log     [u] Undo this action
```

#### ৮.৩.৫ Raw Command Mode ও Danger Detection

Raw, valid শেল syntax দিলে AI সম্পূর্ণ silent থাকে, সরাসরি pass-through execute হয় — কোনো plan, কোনো confirmation:
```
downloads ❯ ls -la
[সরাসরি output]
```
Interactive full-screen প্রোগ্রাম (`vim`, `top`, `htop`) চালু হলে pty পুরোপুরি সেই প্রোগ্রামের হাতে ছেড়ে দেওয়া হয় — কোনো wrapper/capture ছাড়া, যাতে screen-rendering ভেঙে না যায়।

যদি danger classifier raw command-কে potentially destructive চিহ্নিত করে:
```
downloads ❯ rm -rf /var/log/*

  ⚠ Potentially destructive command detected

  This will permanently delete all files in /var/log — including
  system logs that may be needed for diagnostics.

  [y] Run anyway   [n] Cancel   [t] Move to trash instead
```
`[t]` অপশন Undo-philosophy-র সাথেই সংগতিপূর্ণ — raw `rm`-এর নিরাপদ বিকল্প (`.trash/`-এ move) সরাসরি অফার করে।

#### ৮.৩.৬ Sudo/Permission Escalation

**শুধু AI-generated plan-এ প্রয়োজন হলে** Oh My Shell নিজস্ব confirmation দেখায়:
```
  ⚠ Next step requires elevated permission
  ┌─────────────────────────────────────────────────────┐
  │  Step 3: Clear system-level cache in /var/cache         │
  │  Reason: this directory is owned by root                │
  │  [Enter] Grant (sudo)   [s] Skip this step   [Esc] Abort  │
  └─────────────────────────────────────────────────────┘
```
`[s] Skip` করলে বাকি সম্পন্ন কাজ রক্ষা পায় (পুরো task abort হয় না) — audit log-এ "১টা step skipped" হিসেবে নোট থাকে।

**ইউজার raw command-এ নিজে explicit `sudo` লিখলে**, সরাসরি OS password-prompt — বাড়তি Oh My Shell confirmation নেই, কারণ ইউজার নিজেই informed সিদ্ধান্ত নিয়েছে।

#### ৮.৩.৭ Undo ও Trash Retention

```
  ✓ Cleanup complete                              [12.3s]
    340 files removed  ·  1.2 GB reclaimed  ·  moved to .trash/
  ▸ [v] View detailed log     [u] Undo this action
```
`[u]` চাপলে confirm করে restore করা হয়:
```
  ↺ Undo: Restore 340 files from .trash/?
  [Enter] Confirm undo   [Esc] Cancel
```
**`.trash/`-এ ফাইল ৮ দিন (configurable, `/config set trash_retention_days`) পর্যন্ত থাকে**, তারপর auto-delete হয়; expire হওয়ার আগে শেল-startup-এ warning দেখানো হয় (`/trash status`, `/trash keep`, `/trash clear` কমান্ড দিয়ে ম্যানেজ করা যায়)।

#### ৮.৩.৮ Slash Command Output — উদাহরণ

```
downloads ❯ /help

  Oh My Shell — Command Reference
  ────────────────────────────────────────────
  Just type naturally:
    "clean up temp files"          →  AI creates a plan
    ls -la, cd, grep ...            →  runs directly, no AI involved

  Slash commands
    /model          Show/change current model
    /undo           Revert the last destructive action
    /log            View audit log
    /capabilities   List what Oh My Shell can do
    /explain        Why did the AI choose that last action?
    /system         Full hardware & shell status
    /config         View or change settings
    /exit           Quit

  Type /help <command> for details.
```

```
downloads ❯ /system

  System Info
  ────────────────────────────────
  OS: Ubuntu 24.04 LTS  ·  CPU: 12 cores, 34% used  ·  RAM: 15.6GB total, 6.1GB used

  Oh My Shell
  ────────────────────────────────
  Active model: qwen3:8b (5.4GB loaded)  ·  Provider: local (Ollama)  ·  uptime 2h 14m
  Session: 7 requests · 1,204 tokens
```
(dGPU-বিহীন মেশিনে API-fallback active থাকলে: `Provider: Google AI Studio API (cloud)`)

সম্পূর্ণ slash-command তালিকা ও launch-time/inline flag Section 8.4-8.5-এ।

#### ৮.৩.৯ Session Exit

```
downloads ❯ exit
  Session summary
  ────────────────────────────────
  8 requests processed  ·  1,240 tokens used  ·  2 files cleaned up
  Goodbye! 👋
```
Pending (unconfirmed) plan থাকা অবস্থায় `exit` করলে সরাসরি discard হয়, কোনো বাড়তি confirmation ছাড়াই (কিছু এখনো ঘটেনি বলে ঝুঁকি নেই)। Execution চলাকালীন `exit` টাইপ করার সুযোগই নেই (prompt inactive, blocking-design অনুযায়ী) — এই অবস্থায় থামানোর একমাত্র পথ `Ctrl+C` (Section 8.3.4)।

### ৮.৪ Slash Commands

```
/help, /help <command>       কমান্ড রেফারেন্স
/model, /model switch,
/model list                   বর্তমান মডেল দেখা/পরিবর্তন (wizard-এর selection UI reuse)
/history                      Session-এর আগের request তালিকা
/undo                         সর্বশেষ destructive action ফিরিয়ে আনা
/trash status/keep/clear      .trash/ স্ট্যাটাস দেখা ও ম্যানেজ করা
/log, /log export             Audit log দেখা/এক্সপোর্ট
/capabilities                 Capability Registry-র তালিকা
/explain                      সর্বশেষ AI decision-এর reasoning
/stats                        Session-এর token usage ও average latency
/system                       পূর্ণ hardware ও শেল স্ট্যাটাস
/config, /config set <k> <v>  সেটিংস দেখা/পরিবর্তন (model-provider সহ)
/clear, /exit                 স্ক্রিন পরিষ্কার / প্রস্থান
```

### ৮.৫ Launch-Time Flags ও Inline Modifiers

**Launch-time (শেল শুরুর আগে, session-ব্যাপী প্রযোজ্য):**
```
oh-my-shell --model=<name>     নির্দিষ্ট মডেল দিয়ে শুরু
oh-my-shell --no-ai            শুধু raw shell mode
oh-my-shell --safe-mode        Danger classifier আরও strict
oh-my-shell --log-level=<lvl>  Audit log detail-level
oh-my-shell --provider=api     Google AI Studio API ব্যবহার (dGPU না থাকলে; Section 7.8)
```

**Inline modifier (একটা নির্দিষ্ট request-এর সাথে):**
```
--dry-run     শুধু plan দেখাবে, execute করবে না
--yes / -y    Low-risk action-এ confirmation স্কিপ
--verbose     Plan-এ actual command template দেখানো
--quiet       Streaming animation বন্ধ, শুধু সারাংশ
```

**নিরাপত্তা রুল:** `--yes`/`-y` high-risk হিসেবে চিহ্নিত কোনো action-এর confirmation বাইপাস করবে না — এটা Danger Classifier-এর (Section 4.2) hard constraint।

---

## ৯. Team Structure — ৫-সদস্যের Module Breakdown

### Module 1 — Intent Parser & AI Orchestration
- Natural-language-to-intent JSON conversion-এর জন্য prompt engineering, JSON-Schema definition
- Local LLM (Ollama) API integration ও Google AI Studio API provider-abstraction (Section 7.8) — `think: false` mandatory, multi-turn session context management
- Streaming response consumption ও live token-count tracking
- Harness Validation Layer (Pydantic schema check, retry-then-unmapped fallback)
- Knowledge base (`knowledge.md`) ডিজাইন, few-shot examples, prompt injection
- Explainability feature implementation

### Module 2 — Capability Engine & Core Shell Logic
- Input Router (raw / natural-language / slash-command classification, bracketed-paste detection)
- `capabilities.json` registry design — action, **static risk-level**, params-schema, description, network/security capability set
- Core capability-গুলোর জন্য শেল-স্ক্রিপ্ট backend (disk cleanup, file organization, process monitoring)
- Raw command pass-through routing, interactive-program (vim/top) pty hand-off
- Launch-time flag ও inline-modifier parsing
- Streaming Executor-এর execution logic (subprocess management, structured event emission)

### Module 3 — Safety, Risk & Permission
- Danger classifier (regex-based, ambiguous case-এ LLM fallback)
- Risk-tiered confirmation flow, `--yes` override-না-করার hard constraint
- Sudo/permission escalation layer — simple explicit "approve in terminal" flow প্রথমে, pty-based automation stretch goal
- Undo/rollback mechanism (`.trash/`), ৮-দিনের retention ও expiry-warning logic
- `Ctrl+C` graceful-interrupt handling (current-operation-completion, double-Ctrl+C force-stop)

### Module 4 — Streaming UI & UX
- First-run setup wizard (system check, Ollama install, arrow-key model selection, dependency install)
- Prompt design (folder-name display, model-tag, inline icon-change on AI-request)
- Streaming Executor-এর UI rendering (progress bar, file/byte/percentage live counters)
- Collapse-to-summary view
- Plan review-এর discussion/adjustment interface, inline diff-note, soft-limit nudge, direct-edit (`[e]`) flow
- Live token-count ও hardware-load indicator (thinking-window-only, tiered GPU detection)
- Slash-command output rendering (`/help`, `/stats`, `/system`, ইত্যাদি)

### Module 5 — Logging, Testing & Documentation
- Audit log সিস্টেম (completed/interrupted/skipped status-সহ), evaluation metrics-এর জন্য প্রয়োজনীয় field
- দৈনিক/সাপ্তাহিক activity report generation
- Integration testing: danger-classifier false positive/negative rate, model benchmarking (৪০-prompt suite মেইনটেইন করা), JSON-parsing failure rate
- User feedback session coordination
- চূড়ান্ত রিপোর্ট কম্পাইলেশন এবং Engineering Principles/Activities ডকুমেন্টেশন

---

## ১০. Timeline

| সপ্তাহ | ফোকাস |
|---|---|
| ১ | Team role finalize; interface contract লক; hands-on LLM benchmarking (সম্পন্ন — Section 7); instructor-এর সাথে rubric scope confirm |
| ২ | Module 1-2-এর কাজ শুরু; JSON-Schema-constrained decoding ও static-risk registry implement করা; initial core capability |
| ৩ | Module 3-4-এর কাজ শুরু (danger classifier, sudo MVP, streaming UI, interrupt-handling prototype); প্রথম integration test |
| ৪ | সব module-এর পূর্ণ integration; sudo escalation flow (MVP); undo/trash mechanism; সময় থাকলে pty automation |
| ৫ | End-to-end testing; evaluation metrics measure করা; সংক্ষিপ্ত user feedback session; bug fix |
| ৬ | Final polish, demo rehearsal, report submission, presentation preparation |

### ১০.১ Single-Developer Build Order

Team submission-এ ৫-সদস্যের module ownership (Section 9) অপরিবর্তিত থাকে, কিন্তু বাস্তবে single-developer build করার সময় dependency অনুযায়ী নিচের linear ক্রম অনুসরণযোগ্য — উপরের সাপ্তাহিক Timeline-এর কাঠামোর ভেতরেই:

1. Project skeleton + `pyproject.toml` + git init (কোডবেস-নিয়ম #3)
2. `config.py` + Config file load/save (Section 5.5)
3. `registry.py` + `capabilities.json` — ৪টা core capability দিয়ে শুরু (Section 5.4)
4. `validation.py` — Pydantic models, Harness Validation Layer
5. `intent_parser.py` — Ollama client integration, JSON-Schema call, `think:false`
6. `router.py` + `main.py` — basic REPL loop, raw pass-through প্রথমে (সবচেয়ে সহজ path)
7. `plan_generator.py` + `discussion.py` — plan preview, confirm/edit flow, প্রথমে plain-text আকারে
8. `danger_classifier.py` — regex rules প্রথমে, LLM fallback পরে
9. `sudo_layer.py` — simple explicit approve-flow (Module 3-এর pty-automation stretch goal-এর আগে)
10. `executor.py` + `trash.py` — execution, undo/rollback
11. `ui/streaming.py`, `ui/prompt.py`, `ui/panels.py` — `rich` দিয়ে visual polish (এতদিন CLI plain-text-এ কাজ করছিল)
12. `audit_log.py` + `hardware.py` — logging ও live hardware indicator
13. `wizard.py` + `install.sh` — সবার শেষে, কারণ বাকি সব কাজ করার পরই first-run UX পালিশ করার মানে আছে
14. Meta-command handler + slash-commands (`/help`, `/model`, ইত্যাদি) — বিদ্যমান core logic-এর উপর thin wrapper

যুক্তি: প্রতিটা ধাপ আগের ধাপের উপর নির্ভরশীল, এবং risk-critical অংশ (validation, danger classifier, sudo, trash) UI polish-এর আগে solid করা হয়েছে।

---

## ১১. Engineering Principles ও Activities

| Attribute | Oh My Shell কীভাবে address করে |
|---|---|
| **Depth of Knowledge** | Shell scripting, file/permission management, capability-registry design, constrained-decoding, ও LLM integration একসাথে combine করা হয়েছে |
| **Conflicting Requirements** | চারটা genuine trade-off চিহ্নিত ও resolve করা হয়েছে (Section 2.3), প্রতিটার সাথে honestly limitation উল্লেখসহ |
| **Project Context** | Permission handling ও system administration শক্তভাবে address করা |
| **Range of Resources** | ১০টা local LLM-এর hands-on comparative benchmarking, একাধিক RAM-tier ও architecture family জুড়ে (৪০-prompt × ৩ রান); Ollama runtime; `pty`-based terminal control; `rich`/`textual`/`questionary` UI framework; JSON-Schema-constrained decoding ও Pydantic validation; বিদ্যমান টুলের সাথে comparative research |
| **Level of Interaction** | Prospective student user-দের সাথে সংক্ষিপ্ত feedback session; internal task coordination GitHub Issues-এ log |
| **Consequences to Society and Environment** | Safety: danger classification, static risk-registry, audit logging, undo/rollback, interrupt-safety — সবকিছু user data ও system integrity protect করে। Sustainability: local/offline inference-এর কম energy footprint। Privacy: audit log locally-stored, user-facing clear অপশন |

### ১১.১ Evaluation Metrics

| Metric | কীভাবে মাপা হবে |
|---|---|
| Intent-parsing accuracy | ৪০-prompt benchmark suite-এর বিরুদ্ধে periodic re-run |
| Out-of-scope (fail-safe) accuracy | নির্দিষ্টভাবে ট্র্যাক করা, কারণ এটা model-ভেদে সবচেয়ে বেশি variance দেখিয়েছে |
| Danger-classifier precision/recall | Curated safe-vs-dangerous command test set |
| End-to-end task success rate | Plan-confirm-execute flow সফলভাবে সম্পন্ন হওয়ার শতাংশ |
| Interrupt-recovery correctness | Undo/resume পরীক্ষা করা আংশিক-সম্পন্ন অপারেশনে |
| Actual RAM usage | Demo মেশিনে পরিমাপ করে registry-র estimate-এর সাথে তুলনা |

---

## ১২. Risk ও Mitigation

| Risk | Mitigation |
|---|---|
| LLM ভুল intent বুঝলে harmful command চালানোর ঝুঁকি | Capability whitelist, static risk-registry, JSON-Schema-constrained output, harness-level validation — কোনো একক স্তরের উপর নির্ভর না করে defense-in-depth |
| Narrow benchmark দিয়ে ভুল model বেছে নেওয়া | ৪০-prompt broad, adversarial-inclusive real-life suite ব্যবহার করা হয়েছে (LFM2.5-8B-A1B-এর out-of-scope দুর্বলতা এভাবেই ধরা পড়েছে) |
| Demo মেশিনের হার্ডওয়্যার/thermal সীমাবদ্ধতা | Gemma4:12b-এর মতো ভারী মডেল বাদ দেওয়া হয়েছে; হালকা fallback (Phi4-mini) প্রস্তুত |
| ৫ জনের মধ্যে module integration drift | Week ১-এই interface contract লক করা; সাপ্তাহিক sync meeting |
| Live demo-তে দুর্ঘটনাক্রমে system damage | Undo/rollback ও interrupt-safety demo sequence-এরই একটা অংশ হিসেবে অন্তর্ভুক্ত |
| Execution মাঝপথে বন্ধ হয়ে corrupted state | Graceful `Ctrl+C` handling, audit log-এ explicit "interrupted" status, undo/resume অপশন |
| Benchmark/testing-এর সময় ডেভেলপমেন্ট hardware-এ thermal overload (repeated auto-shutdown hands-on পরীক্ষায় দেখা গেছে) | Model-testing batch-এ ভাগ করা, temperature-monitoring সহ-চালানো, ভারী মডেল (Gemma4:12b) বেঞ্চমার্ক থেকেই বাদ দেওয়া; ভবিষ্যতে re-verification কম-intensive, spaced-out সময়সূচিতে করা হবে |

---

## ১৩. Report Structure

1. Cover Page
2. Abstract (150–200 শব্দ)
3. Introduction & Objective / Complex Problem Definition
4. Problem Statement এবং Project Scope
5. Feasibility Study & Gap Analysis
6. System Design — Workflow Diagram ও Data Flow Diagram
7. Methodology (benchmark methodology-সহ)
8. Implementation — Setup Procedure, Shell Script Commands, Screenshots, Sample Transcript
9. Handling Conflicting Requirements
10. Discussion of Real-World Context
11. Challenges Faced and Solutions (benchmark iteration, thermal-shutdown learning, architecture pivot থেকে)
12. Stakeholder/User Interaction Summary
13. Societal and Environmental Impact
14. Range of Resources
15. Results, Analysis & Discussion
16. Project Management & Teamwork
17. References

---

## ১৪. Presentation Plan

**Demo Sequence:**
1. First-run setup wizard-এর সংক্ষিপ্ত walkthrough (arrow-key model selection, live progress)
2. একটা নিরাপদ natural-language request — live token-count, plan (hardware-load-সহ), discussion, confirmation, live execution (file/byte counters)
3. একটা raw command সরাসরি pass-through
4. ইচ্ছাকৃতভাবে dangerous command দিয়ে AI intervention
5. Execution মাঝপথে `Ctrl+C` দিয়ে interrupt, তারপর undo demonstration
6. `/help` ও কয়েকটা slash command demonstration

**Speaking Roles:** Module 1 (AI/intent ও benchmark methodology), Module 2 (capability engine), Module 3 (safety ও interrupt-handling demonstration), Module 4 (setup wizard ও UI walkthrough), Module 5 (metrics ও ফলাফল)।

**Visual Aids:** Architecture diagram (Section 4.1), বেঞ্চমার্ক তুলনা টেবিল (Section 7.2), evaluation metrics table (Section 11.1)।

---

## ১৫. Open Questions for Team

এই ব্লুপ্রিন্টে নিচের বিষয়গুলো টিম নিজে সিদ্ধান্ত নিয়ে বন্ধ করতে পারবে না — external confirmation বা implementation-time সিদ্ধান্ত প্রয়োজন:

1. **Rubric applicability confirmation (instructor)** — কোর্সের CEP rubric-এর একাধিক ভার্সন প্রচলিত থাকতে পারে (Section 2.4-এ উল্লিখিত উদাহরণগুলো মূলত network-service-কেন্দ্রিক)। কোন rubric ভার্সন এই ব্যাচ/সেকশনে প্রযোজ্য, এবং একটা single-machine AI shell tool (কোনো dedicated network service ছাড়া) rubric-এর file-system/permission ও security criteria-তে যথেষ্ট কিনা — এই দুটো বিষয় কোর্স instructor-এর সাথে confirm করে নেওয়া দরকার। যতক্ষণ না confirmation আসে, ততক্ষণ Section 6-এর item 12 (network/security capability set) coverage-hedge হিসেবে scope-এ রাখা হয়েছে — instructor confirm করলে এটা trim করা যেতে পারে।
2. **Firewall backend choice** — Section 6-এর item 12-তে বর্ণিত firewall-rule capability কোন backend (`iptables`, `nftables`, নাকি `ufw`) ব্যবহার করবে তা এখনো নির্দিষ্ট করা হয়নি। Module 2 ও Module 3-কে implementation শুরুর আগে এটা ঠিক করতে হবে; target distro (Section 7.7: Ubuntu/Debian-family)-তে সবচেয়ে ubiquitous ও scriptable বিকল্পটাই default হওয়া উচিত।
3. **Google AI Studio API structured-output verification** — API-provider-mode-এ JSON-Schema-constrained decoding (Section 7.4-এর সমতুল্য reliability) hands-on verify করা এখনো বাকি (Section 7.8)। Implementation-এর আগে অন্তত একটা ছোট test-suite দিয়ে confirm করা দরকার এই provider-এ একই static-risk/harness-validation architecture নির্ভরযোগ্যভাবে কাজ করে কিনা।
4. **বাকি টিম-সদস্যদের হার্ডওয়্যার** — Fazle ও Nuhash-এর মেশিন-স্পেসিফিকেশন hands-on confirm করা হয়েছে (Section 7.9); বাকি ২ সদস্যের হার্ডওয়্যার এখনো unconfirmed। Confirm হলে Section 7.9-এর টেবিল আপডেট করা দরকার।

---

## ১৬. Development Workflow / Working Agreement

এই সেকশন implementation phase-এ Claude-এর জন্য operating rules নির্ধারণ করে — টিমের কোডিং-standard বা rubric-দাবি না, বরং day-to-day development-এ কীভাবে কাজ এগোবে তার working agreement।

1. **কখনো zip ফাইল না** — code সবসময় আলাদা individual file হিসেবে থাকবে।
2. **প্রতিটা code change-এর পর ঠিক ৩ লাইনের git command** — `git add .`, সংক্ষিপ্ত এক-লাইন commit message, `git push`।
3. **নতুন কোডবেস শুরুর সময়** — আগে tools/setup command ও professional file structure (Section 5.2 দেখুন), তারপর কোড।
4. **কখনো guess-ভিত্তিক কোড না** — verify করে, type-check/lint/build চালিয়ে।
5. **Ambiguous/non-trivial সিদ্ধান্তে discuss করা হয়** — একতরফাভাবে অনুমান করে এগোনো হয় না।
6. **Dev environment:** Ubuntu, VS Code, Git Bash।
7. **Commit hygiene** — scoped commit, কখনো secret hardcode না, destructive operation-এর আগে confirm।
8. **Code style** — existing convention মেনে; non-obvious সিদ্ধান্তে সংক্ষিপ্ত "কেন" comment।
