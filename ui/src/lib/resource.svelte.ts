// Data loaded from the API, in one of the four states every view designs for: loading, empty
// (decided by the view), full, and error with the API's own message.
import { ApiError } from "../api/client";

export class Resource<T> {
  data = $state<T | null>(null);
  error = $state<ApiError | null>(null);
  loading = $state(true);
  #load: () => Promise<T>;
  #seq = 0;

  constructor(load: () => Promise<T>, start = true) {
    this.#load = load;
    if (start) void this.reload();
    else this.loading = false;
  }

  /** Loads again; `quiet` keeps what is shown while it loads (polling). */
  async reload(quiet = false): Promise<void> {
    const seq = ++this.#seq;
    if (!quiet) this.loading = true;
    try {
      const data = await this.#load();
      if (seq !== this.#seq) return;
      this.data = data;
      this.error = null;
    } catch (e) {
      if (seq !== this.#seq) return;
      this.error = e instanceof ApiError ? e : new ApiError(0, "error", String(e));
    } finally {
      if (seq === this.#seq) this.loading = false;
    }
  }
}

/** An action's state: running, its error, and a way to run it. */
export class Action {
  running = $state(false);
  error = $state<ApiError | null>(null);

  async run<R>(work: () => Promise<R>): Promise<R | undefined> {
    this.running = true;
    this.error = null;
    try {
      return await work();
    } catch (e) {
      this.error = e instanceof ApiError ? e : new ApiError(0, "error", String(e));
      return undefined;
    } finally {
      this.running = false;
    }
  }
}
