import { test } from "node:test";
import assert from "node:assert/strict";
import maintain from "./dependency-updates.cjs";

function fixture(options = {}) {
  const state = { actor: "dependabot[bot]", source: "usefossa/backend", security: true, file: "Cargo.lock", ci: "success", status: "success", head: "abc", ...options };
  const calls = [];
  const pull = { number: 1, labels: state.security ? [{ name: "security" }] : [], user: { login: state.actor }, head: { repo: { full_name: state.source }, ref: "dependabot/security", sha: "abc" } };
  const github = {
    paginate: async route => ({ pulls: [pull], files: [{ filename: state.file }], checks: [{ name: "ci", app: { id: 15368 }, status: "completed", conclusion: state.ci }] })[route],
    rest: {
      pulls: { list: "pulls", listFiles: "files", get: async () => ({ data: { state: "open", head: { sha: state.head }, base: { ref: "main" } } }), merge: async args => { calls.push(args); if (state.error) throw Object.assign(new Error("blocked"), { status: state.error }); return { data: { merged: true } }; } },
      checks: { listForRef: "checks" },
      repos: { getCombinedStatusForRef: async () => ({ data: { state: state.status, statuses: [] } }) },
      actions: { createWorkflowDispatch: async args => { calls.push(args); } },
    },
  };
  return { calls, input: { github, context: { repo: { owner: "usefossa", repo: "backend" } }, core: { info() {}, notice() {} }, workflow: "deploy.yml" } };
}

test("security fix merges only checked SHA and explicitly requests deployment", async () => {
  const { calls, input } = fixture();
  assert.equal(await maintain(input), 1);
  assert.equal(calls[0].sha, "abc");
  assert.equal(calls[0].commit_message, "");
  assert.equal(calls[1].workflow_id, "deploy.yml");
});
for (const options of [{ actor: "someone" }, { source: "other/backend" }, { security: false }, { file: "src/wallet.rs" }, { ci: "failure" }, { ci: null }, { head: "changed" }]) {
  test(`failed, stale or unrelated update stays open: ${JSON.stringify(options)}`, async () => {
    const { calls, input } = fixture(options);
    assert.equal(await maintain(input), undefined);
    assert.equal(calls.length, 0);
  });
}
test("branch rules and unresolved findings remain mandatory", async () => {
  const { calls, input } = fixture({ error: 405 });
  assert.equal(await maintain(input), undefined);
  assert.equal(calls.length, 1);
});
test("public repository updates never trigger package publishing", async () => {
  const { calls, input } = fixture({ file: "opencode/bun.lock" });
  input.workflow = "";
  assert.equal(await maintain(input), 1);
  assert.equal(calls.length, 1);
});
