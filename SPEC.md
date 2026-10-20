Agentic Poker Arena
1. What will you make?
We will build a Rust-based platform where poker agents compete against one another in heads-up Texas Hold'em.
We chose Rust for memory safety. The poker engine will define an agent interface requiring implementations of
entry() and on_action(). We will create about six example agents that implement these methods with different
actions and strategies. The intended users are students and developers who want to compare how poker agents
behave and perform.
2. What will you show?
In our live demo, we will run matches between our example agents, show the actions each agent takes, and compare
their performance and final chip counts. We will use reproducible match setups and test predetermined hands with
independently known winners. We will also check that chip totals remain consistent and demonstrate that errors
default to folding.
3. How will you direct agents?
We will make the core design decisions about the agent interface, game rules, evaluation, and error handling. One
decision is how long an agent is allowed to take for an action, including an option to request extra time with a
maximum cap, similar to a time-bank button in real poker. Another is making agent errors default to folding. We will
use AI coding agents to help implement the engine, example strategies, and tests, then review their code and verify
outcomes with controlled matches and automated checks.
4. Where will you start?
We will start in Rust and evaluate existing open-source poker libraries for hand evaluation, documenting any reused
code and dependencies. Our main work will be the poker engine, the required entry() and on_action() interface,
roughly six strategy agents, and match performance reporting. The main risks are implementing betting and timing
rules correctly and handling invalid agent behavior. If needed, our fallback is a command-line version of the engine
that runs the six agents, records final chip counts, and includes reproducible tests rather than a web interface.
Scope and reuse: We will not use any course homework material. We will credit external libraries and document our
contributions and agent assistance in the repository.
