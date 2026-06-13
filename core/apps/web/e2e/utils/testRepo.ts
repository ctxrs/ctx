import { execSync } from "child_process";
import { mkdirSync, mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";

type RepoFile = {
  path: string;
  content: string;
};

type CreateTempGitRepoOptions = {
  branch?: string;
  files?: RepoFile[];
  prefix?: string;
};

export function createTempGitRepo(opts: CreateTempGitRepoOptions = {}): string {
  const repo = mkdtempSync(path.join(tmpdir(), opts.prefix ?? "ctx-e2e-repo-"));
  const branch = opts.branch ?? "main";
  execSync(`git init -b ${branch}`, { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  const files = opts.files && opts.files.length > 0 ? opts.files : [{ path: "README.md", content: "fixture\n" }];
  for (const file of files) {
    const absolutePath = path.join(repo, file.path);
    mkdirSync(path.dirname(absolutePath), { recursive: true });
    writeFileSync(absolutePath, file.content);
  }
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });
  return repo;
}
