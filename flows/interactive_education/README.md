# Interactive education

The learning harness behind `learn`, the learning app (`docs/learn.md`): learners aged 13 and up talk to its orb on a tablet, phone or laptop at home, and it answers by voice. Two models work together. The voice listens and talks; the brain, a Wisp agent with this flow's prompt, thinks and writes the answers the voice says.

- `BRAIN.md`: the brain's prompt, loaded as the main agent's preset.
- `VOICE.md`: the voice's instructions, sent when a call opens.
- `settings.toml`: the flow's settings, which `learn` reads in place over your own Wisp config: the brain's preset and its one tool, no project instructions, the voice's instructions, and effort low where nothing else sets one.
- `.gitignore`: keeps out of commits the config, accounts and certificate that earlier setups made here.

The flow holds no code: `learn` lives in `apps/learn`, and a prompt edit here applies at its next start.
