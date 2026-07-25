## Grant Title

Grant Application - Shepherd: Programmable Blockchain Automation Infrastructure for CoW Protocol

## Author(s)

**mfw78.eth**

* GitHub: https://github.com/mfw78 and https://github.com/nxm-rs
* Gitcoin: https://gitcoin.co/mfw78
* Mirror: https://mirror.xyz/mfw78.eth
* Discord: @mfw78

## Experiences and Qualifications

**Relevant Experience:**

* Former CoW Protocol Core Contributor (Aug 2023 - Oct 2024)
* Author: ComposableCoW conditional order framework
* Author: TWAP orders implementation
* Author: ComposableCoW SDK
* Author: Fee Automation architecture (multi-chain bridging, automation)

**Technical Background:**

* Deep knowledge of CoW Protocol architecture and conditional orders
* Experience building watch-tower infrastructure for TWAP monitoring
* Account integration and security (scoped permissions, modular architecture)
* Multi-chain automation
* Smart contract development (Solidity, Foundry)
* Backend development (TypeScript, Rust)

**Why I’m Uniquely Qualified:**

This grant is the natural progression: From building conditional orders (ComposableCoW) → specific order types (TWAP) → now building programmable infrastructure to execute and monitor them flexibly.

## Type of Grant

This is a **milestone-based grant** with 5 distinct milestones, each with clearly defined deliverables and success criteria.

## Grant Description

Shepherd is a **foundational programmable automation infrastructure** for CoW Protocol that replaces the current hardcoded watch-tower with a flexible WASM runtime. It serves as the base layer for all future automation needs—from simple TWAP monitoring to complex multi-chain operations, fee automation, and gas abstraction services.

**The Evolution:**

* **ComposableCoW** (2023): Built the conditional order framework
* **TWAP Orders**: Implemented specific order types
* **Watch-Tower**: Created hardcoded monitoring infrastructure (inflexible)
* **Shepherd**: Provides programmable foundation for ALL automation needs

Instead of rebuilding infrastructure for each new automation pattern, Shepherd provides a secure sandbox where developers deploy WASM modules with well-defined APIs for blockchain interaction (e.g., `eth_call`, `eth_subscribe`), CoW Protocol order submission (permissionless submission to CoW API), and state management (key-value store). The APIs listed throughout this proposal are demonstrative examples, not an exhaustive specification.

**Initial Use Cases:**

* Replace existing TWAP watch-tower with a WASM module
* Replace existing Ethflow monitoring with a WASM module
* Foundation for future automation: Methane gas abstraction (monitor gas prices, trigger paymasters), Fee Automation system (automated bridging, TWAP execution)

**Foundation for Future Automation:**

* Protocol fee collection and management
* Cross-chain bridge monitoring
* Gas market automation
* Yield farming automation
* DAO governance automation
* Any future automation needs

**Key Insight:** Once Shepherd is built, future automation projects (like Methane gas abstraction and Fee Automation) become WASM modules instead of separate infrastructure—faster to build, easier to maintain, more composable.

---

## Problem Statement

### Current Watch-Tower Limitations

CoW Protocol’s existing watch-tower infrastructure is purpose-built for TWAP orders:

* **Hardcoded logic:** Cannot easily adapt for new order types
* **Not extensible:** Community cannot build custom automation
* **Difficult to iterate:** Infrastructure changes required for improvements
* **Single use case:** Only TWAP monitoring, not reusable

### Broader DeFi Automation Gap

Many DeFi automation use cases require monitoring + execution patterns:

* Stop-loss and take-profit orders
* Automated portfolio rebalancing
* Yield farming compounding
* Cross-chain arbitrage
* DAO governance automation

**Current solutions are inadequate:**

* **Gelato/Chainlink Keepers:** Centralised, expensive, limited programmability
* **Custom infrastructure:** Expensive to build, maintain, secure
* **Manual monitoring:** Not scalable, error-prone

### The Missing Piece

We have: ✅ On-chain conditional orders (ComposableCoW) ⚠️ Fixed-case off-chain execution layer (watch-tower and EthFlow)

We’re missing: ❌ **Programmable off-chain execution layer**

Shepherd fills this gap and becomes the foundation for future innovations like gas abstraction (Methane).

---

## Solution: Shepherd Architecture

### High-Level Design

**WASM-Based Runtime:**

* Load and execute WebAssembly modules (“shepherds”)
* Sandboxed execution (CPU/memory/storage limits)
* Automatic restart and error handling
* Flexible deployment across chains

**Well-Defined APIs for WASM Modules:**

```rust
// Event handling
fn on_event(event_type: EventType, event_data: &[u8])

// Blockchain reads
fn eth_call(chain_id, to, data) -> Result<bytes>
fn eth_get_logs(chain_id, filter) -> Result<Vec<Log>>

// Blockchain writes
fn eth_send_transaction(chain_id, tx) -> Result<TxHash>

// State management (persistent key-value store)
fn state_get(key) -> Result<Option<bytes>>
fn state_set(key, value) -> Result<()>
```

**Event Sources:**

* `eth_subscribe`: Real-time blockchain events (new blocks, logs)
* Timer events: Cron-like scheduling
* Custom triggers: Extensible for future needs

### How It Works

**Example: TWAP Monitoring Module**

1. **Module subscribes to new blocks** on Arbitrum
2. **On each block**, module queries ComposableCoW for active TWAP orders
3. **Checks state store** to see which parts have been posted
4. **Calculates** if next TWAP part is due based on time elapsed
5. **Submits order** to CoW Protocol API for execution
6. **Updates state** to mark part as posted

**All of this logic is in WASM**, easily updatable without infrastructure changes.

### Key Innovations

1. **WASM Sandboxing:** Secure execution of community modules
2. **Stateful Automation:** Key-value store for persistent data
3. **Chain-Flexible:** Modules can be deployed per chain or configured for multiple chains
4. **Composable:** Modules are reusable building blocks
5. **Self-Hosted:** No centralised service, full control

---

## Use Cases: Shepherd as Foundation

### Core Protocol Use Cases (Enabled by Shepherd)

**1. TWAP Order Monitoring (Initial Implementation)** Replace current watch-tower with WASM module. Easy to update, community can customise.

**2. Ethflow Order Monitoring (Initial Implementation)** Replace existing Ethflow monitoring with WASM module, removing Ethflow-specific logic from the backend and reducing cross-domain concerns.

**3. Methane Gas Abstraction (Future - Built on Shepherd)**

* **Module:** Monitor gas prices across networks
* **Module:** Track paymaster ETH reserves
* **Module:** Trigger rebalancing when reserves low
* **Module:** Optimise gas recovery batching
* Leverages Shepherd’s multi-chain monitoring capabilities

**4. Fee Automation System (Future - Built on Shepherd)**

* **Module:** Monitor Fee account balances across all CoW Protocol networks
* **Module:** Trigger bridge operations when thresholds met
* **Module:** Calculate weekly COW payout amounts
* **Module:** Initiate TWAP buyback orders
* All implemented as Shepherd WASM modules with shared state

### Community Use Cases

**5. Stop-Loss / Take-Profit Orders** Monitor price oracles, submit CoW order when conditions met.

**6. Automated Portfolio Rebalancing** Track wallet balances, trigger rebalance when allocation drifts.

**7. Yield Farming Automation** Monitor lending positions, automatically compound rewards when profitable.

**8. DAO Governance Automation** Automatically vote on proposals based on predefined rules.

**Key Point:** Shepherd is the **foundation**. Once built, Methane and Fee Automation become modules rather than separate infrastructure projects—dramatically reducing complexity and development time.

---

## Technical Approach

### Technology Stack

**Runtime:**

* **Language:** Rust
* **WASM Runtime:** wasmtime (secure, sandboxed, production-ready)
* **Async:** Tokio
* **RPC:** alloy
* **Database:** redb (embedded key-value store, no external dependencies)
* **Logging:** Structured JSON logs (Prometheus-compatible metrics)

**SDK:**

* Rust SDK for WASM module development
* Types, traits, and macros for easy development
* Testing utilities and examples

**Deployment:**

* Docker container
* CLI for local testing and deployment
* Configuration via TOML files

### Security Model

**WASM Sandboxing:**

* No file system access
* No network access (all RPC via runtime)
* CPU/memory/storage limits enforced
* Module crashes don’t crash runtime

**Order Submission:**

* **For CoW Protocol:** Orders submitted via permissionless API (no authentication required)
* Orders do not require on-chain transactions
* **Future with Methane:** Shepherds can leverage ERC-4337 wallets with on-chain verification for more complex operations

**Audit Trail:**

* All events, state changes, and order submissions logged
* Structured logs for observability

---

## Milestones and Deliverables

### Milestone 1: Core Runtime & Event System

**Duration:** 3 weeks **Effort Estimate:** 120 hours (3 weeks FTE)

**Deliverables:**

* WASM runtime with lifecycle management (load, execute, restart)
* Event monitor supporting `eth_subscribe` (new blocks and logs)
* Basic blockchain interface (`eth_call`, `eth_getTransactionReceipt`)
* redb-backed state store with per-module isolation
* CLI for testing WASM modules locally
* Example “hello world” module that responds to new blocks

**Success Criteria:**

* Can load WASM module and receive new block events
* Module can read blockchain state via `eth_call`
* Module can persist data in state store
* No memory leaks or crashes over 24-hour test run

---

### Milestone 2: TWAP & Ethflow Module Implementation

**Duration:** 2.5 weeks **Effort Estimate:** 100 hours (2.5 weeks FTE)

**Deliverables:**

* Complete TWAP monitoring module in Rust (compiled to WASM)
* Complete Ethflow monitoring module in Rust (compiled to WASM)
* **Smart contract modifications to ComposableCoW/TWAP handler:**
  * Enhanced polling interfaces for efficient order discovery
  * Optimised getter functions for active TWAP parts
  * Events for better monitoring capabilities
* Integration with modified ComposableCoW to query active orders
* Logic to calculate when TWAP parts are due
* Order submission to CoW Protocol API (permissionless)
* State tracking to avoid duplicate posts
* Ethflow order monitoring and submission logic

**Success Criteria:**

* Modified ComposableCoW contracts deployed and tested
* TWAP module successfully monitors and posts orders on Arbitrum testnet
* Ethflow module successfully monitors and posts orders on testnet
* No duplicate order posts
* Handles edge cases (reorgs, failed order submissions)
* 100% uptime over 48-hour test period

---

### Milestone 3: SDK & Developer Experience

**Duration:** 1.5 weeks **Effort Estimate:** 60 hours (1.5 weeks FTE)

**Deliverables:**

* Rust SDK crate (`shepherd-sdk`) with types and utilities
* Macros for common patterns (event handling, state management)
* Testing framework for WASM modules
* Example modules:
  * TWAP monitor (reference implementation)
  * Simple price alert (demonstrates oracle reading)
  * Balance tracker (demonstrates state usage)
* Documentation:
  * API reference
  * Module development tutorial
  * Deployment guide

**Success Criteria:**

* External developer can build simple module in <4 hours using docs
* SDK has clear error messages
* All examples compile and run

---

### Milestone 4: Production Hardening

**Duration:** 1.5 weeks **Effort Estimate:** 60 hours (1.5 weeks FTE)

**Deliverables:**

* Resource limits (CPU time, memory, storage) with enforcement
* Automatic restart on module crashes with exponential backoff
* Poison pill detection (modules that always crash)
* Comprehensive logging (all events, state changes, order submissions)
* Prometheus metrics export (uptime, event latency, error rates)
* Production deployment guide

**Success Criteria:**

* System runs for 7 days on testnet without manual intervention
* Handles RPC failures gracefully (auto-retry, fallback)
* Module crashes don’t affect runtime stability
* All operations logged and observable

---

### Milestone 5: Multi-Chain Considerations & Final Testing

**Duration:** 1 week **Effort Estimate:** 40 hours (1 week FTE)

**Deliverables:**

* Documentation for multi-chain deployment patterns
* Support for configuring RPC endpoints (Arbitrum, Base, Gnosis, Mainnet)
* Module chain configuration capabilities
* Docker image for deployment
* Comprehensive deployment documentation covering:
  * Module development
  * Local testing
  * Deployment to production (single or multi-chain setups)
  * Monitoring and observability

**Success Criteria:**

* Clear documentation for deploying Shepherd across multiple chains
* Flexible architecture supporting both single-chain and multi-chain deployments
* Docker image runs on fresh server with minimal configuration
* Complete documentation for production deployment

---

## Total Effort Estimate

**Total Duration:** 9.5 weeks **Total Effort:** 380 hours (9.5 weeks FTE)

**Breakdown:**

| Milestone | Duration | Effort |
|----|----|----|
| 1 | 3 weeks | 120 hours |
| 2 | 2.5 weeks | 100 hours |
| 3 | 1.5 weeks | 60 hours |
| 4 | 1.5 weeks | 60 hours |
| 5 | 1 week | 40 hours |
| **Total** | **9.5 weeks** | **380 hours** |

---

## Grant Funding Request

**Rate:** €100/hour **Total Grant Amount:** €38,000

**Payment Terms:**

* **25% up-front:** €9,500 (upon grant approval)
* **75% on completion:** €28,500 (upon successful delivery of all milestones)

**Breakdown by Milestone:**

| Milestone | Effort | Cost (€100/hr) |
|----|----|----|
| 1 | 120 hours | €12,000 |
| 2 | 100 hours | €10,000 |
| 3 | 60 hours | €6,000 |
| 4 | 60 hours | €6,000 |
| 5 | 40 hours | €4,000 |
| **Total** | **380 hours** | **€38,000** |

---

## Gnosis Chain Address (to receive the grant)

`0xc0de401Dfb531Ec15A453C3301E5807Cf2C8323e`

---

## Length

**Expected Completion:** 9 weeks from commencement date

**Commencement Date:** Upon successful passing of the proposal on Snapshot

**Final Delivery Date:** Approximately 2 months from commencement (9 weeks)

This grant will be completed well within the 6-month maximum timeframe for CoW Protocol grants.

---

## Value to CoW Protocol Ecosystem

### Immediate Benefits

1. **Better TWAP Infrastructure**

   * Replace hardcoded watch-tower with flexible WASM module
   * Easy to update and improve
   * Community can fork and customise

2. **Streamlined Backend Architecture**

   * Eliminate Ethflow-specific monitoring logic from backend
   * Reduces cross-domain concerns and architectural complexity
   * Allows backend to focus on core protocol logic
   * Improves maintainability and separation of concerns

3. **Platform for Innovation**

   * Enable novel order types without infrastructure changes
   * Community can build automation patterns
   * Accelerate experimentation

4. **Competitive Differentiation**

   * Most DeFi protocols don’t have programmable automation
   * CoW becomes platform for sophisticated trading strategies
   * Attracts developer mindshare

### Long-Term Strategic Value

1. **Ecosystem Growth**

   * Module marketplace (community-built automation)
   * Integration partners building on Shepherd
   * Network effects from shared infrastructure

2. **Protocol Sustainability**

   * Automation increases CoW Protocol usage
   * More complex strategies = more volume
   * Self-service automation reduces support burden

3. **Foundation for Future Features**

   * Gas abstraction integration (Methane + Shepherd)
   * Cross-chain automation (Fee Automation patterns)
   * Community-driven automation innovations

---

## Comparison to Existing Solutions

| Feature | Shepherd | Gelato | Chainlink Keepers | OpenZeppelin Defender |
|----|----|----|----|----|
| **Hosting** | Self-hosted | Centralised | Centralised | Centralised |
| **Cost** | Free (gas only) | Per-transaction fee | Per-transaction fee | Subscription |
| **Programmability** | Full (WASM) | Limited | Limited | Limited |
| **Open Source** | Yes | No | No | No |
| **Community Extensible** | Yes | No | No | No |
| **CoW Integration** | Native | Generic | Generic | Generic |

**Shepherd is the only fully programmable, self-hosted, community-extensible automation platform.**

---

## Risks and Mitigations

### Risk 1: WASM Performance Overhead

**Mitigation:** Benchmark early, optimise hot paths, profile memory usage. WASM is near-native performance for computational tasks.

### Risk 2: RPC Rate Limiting

**Mitigation:** Implement caching, request batching, exponential backoff. Support multiple RPC endpoints with automatic failover.

### Risk 3: Module Security (Malicious WASM)

**Mitigation:** Strong sandboxing via wasmtime, strict resource limits, code review for “official” modules. Clear documentation on running untrusted modules.

### Risk 4: State Corruption

**Mitigation:** Transactional state updates, regular backups, state versioning. redb provides ACID guarantees with embedded key-value storage.

### Risk 5: Complexity for Developers

**Mitigation:** Comprehensive SDK, clear examples, good documentation, interactive tutorials. Make common patterns simple.

---

## Open Source Commitment

**All code will be released under AGPL open source license per CoW DAO grant requirements.**

**Repositories:**

* `shepherd-runtime`: Core WASM runtime (Rust)
* `shepherd-sdk`: SDK for module development (Rust)
* `shepherd-modules`: Official modules (TWAP, EthFlow, examples)

**License:** AGPL-3.0

---

## Success Metrics

### Technical Success

* ✅ TWAP module achieves 99%+ uptime on mainnet
* ✅ <1 second latency from event to module callback
* ✅ <10MB memory per module
* ✅ Zero security incidents

### Adoption Success

* ✅ Replaces existing TWAP watch-tower in production
* ✅ 3+ community modules within 3 months of launch
* ✅ Used for at least one other CoW automation (beyond TWAP)
* ✅ Positive developer feedback on SDK/docs

---

## Future Enhancements (Post-Grant)

### Shepherd-Methane Synergy: Gasless On-Chain Automation

**Once both Shepherd and Methane are deployed:**

Shepherd modules can leverage Methane for completely trustless, gasless operation:

1. **Each Shepherd module gets an ERC-4337 wallet**

   * No EOA needed (no signing keys to manage)
   * On-chain verification logic for module actions
   * Fully deterministic and verifiable

2. **Methane handles gas abstraction**

   * Shepherd modules pay gas in COW/USDC/DAI
   * No ETH reserves needed for modules
   * Gas costs covered by protocol or module fees

3. **True on-chain automation**

   * Modules execute on-chain transactions (not just API calls)
   * Bridge operations, token transfers, contract interactions
   * All without managing private keys

**Example Flow:**

```
Shepherd Module (WASM)
→ Determines action needed
→ Creates ERC-4337 UserOperation
→ Signs with on-chain logic (no EOA)
→ Methane Paymaster sponsors gas
→ Transaction executes on-chain
```

This creates the most powerful automation infrastructure in DeFi: fully programmable, gasless, and trustless.

### Phase 2 Ideas (Unfunded, Potential Follow-Up)

1. **Multi-Language Support**

   * TypeScript/AssemblyScript SDK
   * Go and C++ module support

2. **Distributed Deployment**

   * Multi-node setup with leader election
   * Shared state backend (Postgres)
   * High availability

3. **Module Marketplace**

   * Registry of community modules
   * Versioning and updates
   * Reputation system

4. **Advanced Event Sources**

   * GraphQL subscriptions (The Graph)
   * Custom webhooks
   * Off-chain data feeds

5. **GUI Configuration**

   * Web UI for module deployment
   * Visual monitoring dashboard

*These are potential future directions, not included in current grant scope.*

---

## Team and Commitment

**Primary Developer:** mfw78

**Availability:** Committed to delivering all milestones within the specified 9-week timeline. Project effort estimated at 380 hours (FTE equivalent).

**Track Record:**

* ✅ ComposableCoW: Delivered on time, production-ready
* ✅ TWAP Orders: Live on mainnet, processing significant volume
* ✅ ComposableCoW SDK: Widely adopted by developers
* ✅ Core Contributor: 14+ months of consistent contributions

**Why This Project:** Shepherd represents the culmination of 18+ months working on CoW Protocol automation:

1. Built the conditional order framework (ComposableCoW)
2. Implemented specific order types (TWAP)
3. Learned watch-tower pain points firsthand
4. Now building the programmable solution I wish existed from the start

This is the natural next step in the progression.

---

## Communication and Reporting

**Updates:** Weekly progress reports posted to CoW Protocol forum

**Communication Channels:**

* CoW Protocol Discord
* Grant application forum thread

**Transparency:**

* All code public from day 1
* Issues and PRs visible
* Architecture decisions documented

---

## Conflict of Interest Disclosure

**Previous Work:** Core contributor to CoW Protocol (Aug 2023 - Oct 2024), funded by previous grants.

**Current Position:** Active member of the CoW DAO Grants Committee. I will abstain from voting on this proposal to eliminate conflicts of interest.

**Current Relationship:** No ongoing financial relationship with CoW Protocol. Applying for this grant as independent contributor.

**No Conflicts:** Not employed by competitors, no conflicting financial interests.

---

## Conclusion

Shepherd completes the automation story for CoW Protocol:

**The Journey:**

1. **ComposableCoW** → Conditional orders on-chain ✅
2. **TWAP/SDK** → Specific implementations + developer tools ✅
3. **Fee Automation/Methane** → Advanced automation patterns ✅
4. **Shepherd** → Programmable execution layer ✅

**The Vision:** Make CoW Protocol the platform for programmable DeFi automation—where any developer can build sophisticated trading strategies, yield optimisation, and automation without building infrastructure from scratch.

**Why Now:**

* The conditional order framework is mature
* The security model for automation is proven
* The watch-tower pain points are well-understood
* The ecosystem is ready for community extensibility

**Total Effort:** 380 hours over 9.5 weeks to deliver production-ready programmable automation infrastructure.

---

## Terms and Conditions

By submitting this grant application, I acknowledge and agree to be bound by the [CoW DAO Participation Agreement](https://gateway.pinata.cloud/ipfs/Qmf9MYhcG2pFrDoVy13p6FWeVF4nG9HbJvRfYYbhazTCFe) and the [CoW DAO Grant Agreement Terms](https://bafkreifcftgaleyxkekkic36beyveiomqmlwyduyfh3s25zj3uyngr6ht4.ipfs.dweb.link/).

**NOTE TO COMMITTEE:**

Please notify the Grantee of their reviewer and their steward in the thread and latest upon successful approval of the Grant on Snapshot.

---

**Thank you for considering this grant application. I’m excited to build Shepherd and unlock the next wave of innovation on CoW Protocol.**
