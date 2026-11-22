import { constants } from "node:fs";
import { open, readdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const OUTPUT_LIMIT = 16 * 1024;

export async function readOutput(file) {
  if (!file) return null;
  try {
    const handle = await open(file, constants.O_RDONLY | constants.O_NONBLOCK);
    try {
      const info = await handle.stat();
      if (!info.isFile()) return null;
      const buffer = Buffer.alloc(Math.min(info.size, OUTPUT_LIMIT));
      const { bytesRead } = await handle.read(buffer, 0, buffer.length, info.size - buffer.length);
      return buffer.toString("utf8", 0, bytesRead);
    } finally {
      await handle.close();
    }
  } catch {
    return null;
  }
}

export async function liveOutput(cwd, sessionId, taskId) {
  if (typeof process.getuid !== "function" || ![sessionId, taskId].every((id) => /^[a-z0-9_-]+$/i.test(id))) {
    return null;
  }
  // Claude's live output directory is private to the CLI. The final notification supplies its own path.
  const base = process.env.CLAUDE_CODE_TMPDIR?.trim() || (process.platform === "darwin" ? "/tmp" : tmpdir());
  const root = resolve(cwd, base, `claude-${process.getuid()}`);
  const projects = await readdir(root, { withFileTypes: true }).catch(() => []);
  for (const project of projects) {
    if (!project.isDirectory()) continue;
    const output = await readOutput(join(root, project.name, sessionId, "tasks", `${taskId}.output`));
    if (output !== null) return output;
  }
  return null;
}

export class BackgroundShells {
  tasks = new Map();
  active = new Map();

  update(message) {
    const id = message.task_id;
    if (message.subtype === "background_tasks_changed") {
      this.active = new Map();
      for (const task of message.tasks) {
        if (task.task_type !== "local_bash" || task.ambient) continue;
        const shell = { id: task.task_id, label: task.description, run: task.run_id, backgrounded: true };
        this.tasks.set(shell.id, shell);
        this.active.set(shell.id, shell);
      }
      return true;
    }
    if (!id) return false;
    const known = this.tasks.get(id);
    if (known?.run && message.run_id && message.run_id < known.run) return false;
    if (message.subtype === "task_started" && message.task_type === "local_bash" && !message.ambient) {
      const shell = { id, label: message.description, run: message.run_id, backgrounded: message.is_backgrounded || known?.backgrounded === true };
      this.tasks.set(id, shell);
      if (message.is_backgrounded) this.active.set(id, shell);
      return message.is_backgrounded === true;
    }
    if (message.subtype === "task_updated" && known && message.patch.is_backgrounded) {
      known.backgrounded = true;
      this.active.set(id, known);
      return true;
    }
    return false;
  }

  finish(message) {
    const id = message.task_id;
    const shell = this.tasks.get(id);
    if (!shell || (shell.run && message.run_id && shell.run !== message.run_id)) return null;
    this.tasks.delete(id);
    this.active.delete(id);
    return shell;
  }
}
