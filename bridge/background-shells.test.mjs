import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { BackgroundShells, readOutput } from "./background-shells.mjs";

test("a shell that moves to the background retains its identity through completion", () => {
  const shells = new BackgroundShells();
  const start = { subtype: "task_started", task_id: "shell", task_type: "local_bash", run_id: "2", description: "Tests" };
  assert.equal(shells.update(start), false);
  assert.equal(shells.active.size, 0);
  assert.equal(shells.update({ subtype: "task_updated", task_id: "shell", run_id: "2", patch: { is_backgrounded: true } }), true);
  assert.equal(shells.active.get("shell").label, "Tests");
  assert.equal(shells.finish({ task_id: "shell", run_id: "1" }), null);
  assert.equal(shells.active.size, 1);
  assert.equal(shells.update({ subtype: "background_tasks_changed", tasks: [] }), true);
  assert.equal(shells.active.size, 0);
  assert.equal(shells.finish({ task_id: "shell", run_id: "2" }).backgrounded, true);
});

test("the roster excludes ambient tasks and tasks other than shells", () => {
  const shells = new BackgroundShells();
  shells.update({ subtype: "background_tasks_changed", tasks: [
    { task_id: "one", task_type: "local_bash", description: "Build" },
    { task_id: "two", task_type: "local_bash", ambient: true },
    { task_id: "three", task_type: "local_agent" },
  ] });
  assert.deepEqual([...shells.active.keys()], ["one"]);
});

test("output is bounded to the latest 16 KiB of a regular file", async () => {
  const dir = await mkdtemp(join(tmpdir(), "m4code-shell-output-"));
  try {
    const file = join(dir, "shell.output");
    await writeFile(file, `header${"x".repeat(16 * 1024 - 4)}tail`);
    const output = await readOutput(file);
    assert.equal(output.length, 16 * 1024);
    assert.equal(output.slice(-4), "tail");
    assert.equal(await readOutput(dir), null);
    assert.equal(await readOutput(join(dir, "missing")), null);
  } finally {
    await rm(dir, { recursive: true });
  }
});
