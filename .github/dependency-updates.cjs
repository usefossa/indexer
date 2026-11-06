module.exports = async function maintain({ github, context, core, workflow = "", inputs = {} }) {
  const repo = context.repo;
  const pulls = await github.paginate(github.rest.pulls.list, { ...repo, state: "open", base: "main" });
  for (const pull of pulls) {
    if (pull.user.login !== "dependabot[bot]" || pull.head.repo?.full_name !== `${repo.owner}/${repo.repo}` || !pull.head.ref.startsWith("dependabot/") || !pull.labels.some(label => label.name === "security")) continue;
    const request = { ...repo, pull_number: pull.number };
    const [files, checks, status, reviews] = await Promise.all([
      github.paginate(github.rest.pulls.listFiles, request),
      github.paginate(github.rest.checks.listForRef, { ...repo, ref: pull.head.sha, filter: "latest" }),
      github.rest.repos.getCombinedStatusForRef({ ...repo, ref: pull.head.sha }),
      github.paginate(github.rest.pulls.listReviews, request),
    ]);
    if (!files.length || files.some(({ filename }) => !/^(?:(?:[^/]+\/)*(?:package\.json|bun\.lock|Cargo\.(?:toml|lock))|\.github\/workflows\/[^/]+\.ya?ml)$/.test(filename))) continue;
    if (!checks.some(check => check.name === "ci" && check.app?.id === 15368 && check.conclusion === "success") || checks.some(check => check.status !== "completed" || !["success", "skipped", "neutral"].includes(check.conclusion))) continue;
    if (status.data.statuses.length && status.data.state !== "success") continue;
    const review = reviews.findLast(review => review.user.id === 175728472 && review.commit_id === pull.head.sha);
    if (!review || !["COMMENTED", "APPROVED"].includes(review.state)) continue;
    const current = (await github.rest.pulls.get(request)).data;
    if (current.state !== "open" || current.head.sha !== pull.head.sha || current.base.ref !== "main") continue;
    let merged;
    try {
      merged = await github.rest.pulls.merge({ ...request, sha: pull.head.sha, merge_method: "squash", commit_title: "[fix] security dependency updates", commit_message: "" });
    } catch (error) {
      if (![405, 409].includes(error.status)) throw error;
      core.notice(`security pr ${pull.number} remains blocked by branch rules or unresolved findings`);
      continue;
    }
    if (!merged.data.merged) continue;
    // workflow-token merges suppress push workflows; explicitly dispatch the trusted deployment.
    if (workflow) await github.rest.actions.createWorkflowDispatch({ ...repo, workflow_id: workflow, ref: "main", inputs });
    core.info(`merged reviewed security pr ${pull.number}`);
    return pull.number;
  }
};
