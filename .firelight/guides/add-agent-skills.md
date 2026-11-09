---
format: 1
title: Teach your coding agent veld
summary: Install the veld pointer skill with npx skills add, then let the agent read veld agent skills topics from the binary.
aliases: [agent skills, skills, claude code, npx skills add, veld agent skills, veld skill, llms.txt, ai agent]
tags: [agents]
links: [llms-full]
related: [veld-ide, hand-off-to-a-worktree]
covers: [skills/**, crates/veld/src/commands/skills.rs, crates/veld/src/commands/skills/**]
---
1. `npx skills add prosperity-solutions/veld`. It installs one small skill, `veld`.
2. The agent then reads the docs of your installed veld: `veld agent skills` (13 topics),
   `veld agent skills basics`, `veld agent skills config`.
3. Had the old skills? Remove them:
   `npx skills remove veld-launch-feedback-loop veld-feedback`, then add again as in step 1.

## What it does not do
The skill holds no docs itself, so it never goes stale and never needs re-adding.
