// The tenant's jobs, for the tray: polled quickly while one is queued or running, slowly
// otherwise. A job just submitted is shown at once.
import type { JobInfo } from "../api/types";
import { api } from "../api/client";
import { session } from "./session.svelte";

class Jobs {
  list = $state<JobInfo[]>([]);
  #timer: number | undefined;
  #listeners = new Set<(job: JobInfo) => void>();

  get active(): JobInfo[] {
    return this.list.filter((j) => j.status === "queued" || j.status === "running");
  }

  /** Called when a job finishes (once per job seen running). */
  onFinish(f: (job: JobInfo) => void): () => void {
    this.#listeners.add(f);
    return () => this.#listeners.delete(f);
  }

  track(job: JobInfo): void {
    this.list = [job, ...this.list.filter((j) => j.job !== job.job)];
    this.schedule(400);
  }

  start(): void {
    this.schedule(0);
  }

  stop(): void {
    window.clearTimeout(this.#timer);
    this.list = [];
  }

  private schedule(ms: number): void {
    window.clearTimeout(this.#timer);
    this.#timer = window.setTimeout(() => void this.poll(), ms);
  }

  private async poll(): Promise<void> {
    if (!session.signedIn) return;
    const before = new Map(this.list.map((j) => [j.job, j.status]));
    try {
      const { jobs } = await api.jobs();
      this.list = jobs;
      for (const j of jobs) {
        const was = before.get(j.job);
        if ((was === "queued" || was === "running") && (j.status === "done" || j.status === "failed")) {
          for (const f of this.#listeners) f(j);
        }
      }
    } catch {
      // kept as it was; tried again below
    }
    this.schedule(this.active.length > 0 ? 1500 : 15000);
  }
}

export const jobs = new Jobs();
