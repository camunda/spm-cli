// Minimal GitHub REST client (pulls + labels) over fetch: just what the update
// action needs, dependency-free.

const PER_PAGE = 100;
export class GitHub {
  constructor({ apiUrl, token, repository }) {
    if (!/^[^/\s]+\/[^/\s]+$/.test(repository ?? "")) {
      throw new Error(`invalid repository \`${repository}\`: expected owner/name`);
    }
    this.apiUrl = (apiUrl || "https://api.github.com").replace(/\/+$/, "");
    this.token = token;
    this.repository = repository;
    this.owner = repository.split("/")[0];
  }

  async request(method, path, body) {
    const res = await fetch(`${this.apiUrl}${path}`, {
      method,
      headers: {
        Accept: "application/vnd.github+json",
        Authorization: `Bearer ${this.token}`,
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": "camunda/spm-cli update action",
        ...(body === undefined ? {} : { "Content-Type": "application/json" }),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await res.text();
    if (!res.ok) {
      const err = new Error(`${method} ${path}: HTTP ${res.status} ${text.slice(0, 500)}`);
      err.status = res.status;
      throw err;
    }
    return text ? JSON.parse(text) : null;
  }

  /// All pull requests from `branch` in this repository into `base`, every
  /// page of them.
  async listPulls(branch, state, base) {
    const head = encodeURIComponent(`${this.owner}:${branch}`);
    const all = [];
    for (let page = 1; ; page++) {
      const batch = await this.request(
        "GET",
        `/repos/${this.repository}/pulls?state=${state}&head=${head}&base=${encodeURIComponent(base)}&per_page=${PER_PAGE}&page=${page}`,
      );
      all.push(...batch);
      if (batch.length < PER_PAGE) return all;
    }
  }

  createPull(pull) {
    return this.request("POST", `/repos/${this.repository}/pulls`, pull);
  }

  updatePull(number, fields) {
    return this.request("PATCH", `/repos/${this.repository}/pulls/${number}`, fields);
  }

  /// Labels that don't exist yet are created by GitHub on the fly.
  addLabels(number, labels) {
    return this.request("POST", `/repos/${this.repository}/issues/${number}/labels`, { labels });
  }

  async removeLabel(number, label) {
    try {
      await this.request(
        "DELETE",
        `/repos/${this.repository}/issues/${number}/labels/${encodeURIComponent(label)}`,
      );
    } catch (e) {
      if (e.status !== 404) throw e;
    }
  }
}
