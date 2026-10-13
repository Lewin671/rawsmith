# Working on rawsmith

Keep the product reliable for an assistant's complete editing loop: inspect,
start, preview, refine, compare, check detail and export. Read README.md and
CONTRIBUTING.md before making changes. Code and repository documentation
are in English.

- The lead agent owns priorities, integration and final acceptance. Use
  subagents for independent review or bounded implementation when useful;
  give each writer its own branch and worktree. Review their changes and
  validate the combined result before integrating.
- Use Codex exclusively for acceptance: final code review, end-to-end
  editing, visual quality judging and release usability checks. Codex may
  inspect Claude Code installation compatibility, but do not run Claude
  as an acceptance agent or judge. Photo benchmarks default to the repository
  Codex adapter with explicit gpt-6.1-sol model selection and low reasoning
  effort. Verify the adapter, model and effort configuration before using a
  run for acceptance.
  Existing Claude benchmark results are historical evidence, not new
  acceptance. Automated correctness checks remain part of validation.
- Prioritize data loss, misleading success and unusable exports before
  cosmetic improvements. Error paths matter as much as successful edits.
  Invalid commands must preserve existing RAWs, recipes and previews.
- Check CLI behavior, JSON reports, help, README and the editing skill
  together. An assistant must be able to discover valid parameters,
  distinguish trials from saved edits and recover from a failed command.
- For pixel changes, examine representative scenes and full-resolution
  detail as well as synthetic metrics. Include bright snow, dark shadows,
  skin, haze and saturated skies when relevant. A preview alone cannot
  establish final export quality.
- Keep performance claims tied to a measured workload and machine. Follow
  the workspace's shared-host rules; never overlap builds or tests with
  an iMac measurement. Do not run performance measurements locally.
- Keep private photos, renders and identifying paths out of commits and
  public reports unless their publication is explicitly authorized.
  Verify metadata behavior separately for developed images and extracted
  camera JPEGs. Never send photos to a judge without authorization.
- Test confirmed failures with synthetic inputs, run the relevant checks
  in CONTRIBUTING.md, and distinguish verified behavior from remaining
  limitations. Document deferred findings with concrete evidence.
