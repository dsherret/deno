import { Path } from "jsr:@david/path@0.2";

interface TestContextOptions {
  envs: Record<string, string>;
  cwd: string | undefined;
  tempCwd: boolean;
}

export class TestContextBuilder {
  #options: TestContextOptions;

  constructor() {
    this.#options = {
      envs: {},
      cwd: undefined,
      tempCwd: false,
    };
  }

  env(key: string, value: string) {
    this.#options.envs[key] = value;
    return this;
  }

  useTempCwd() {
    this.#options.tempCwd = true;
    return this;
  }

  build() {
    return new TestContext(this.#options);
  }
}

export class TestContext implements Disposable {
  #disposables: Disposable[] = [];
  #cwd: Path;
  #env: Record<string, string>;

  constructor(opts: TestContextOptions) {
    if (opts.tempCwd) {
      const tempDir = new TempDir();
      this.#disposables.push(tempDir);
      this.#cwd = tempDir.path;
    } else {
      this.#cwd = new Path(Deno.cwd());
    }
    if (opts.cwd != null) {
      this.#cwd = this.#cwd.join(opts.cwd);
    }
    this.#env = opts.envs;
  }

  [Symbol.dispose]() {
    for (const disposable of this.#disposables) {
      disposable[Symbol.dispose]();
    }
  }

  get cwd() {
    return this.#cwd;
  }

  newCommand() {
    return new TestCommandBuilder({
      cwd: this.#cwd,
      envs: { ...this.#env },
    });
  }
}

interface TestCommandOptions {
  envs: Record<string, string>;
  cwd: Path;
  command: string;
  args?: string[];
}

export class TestCommandBuilder {
  #options: TestCommandOptions;

  constructor(initialOptions: { envs: Record<string, string>; cwd: Path }) {
    this.#options = {
      envs: initialOptions.envs,
      command: "deno",
      cwd: initialOptions.cwd,
    };
  }

  env(key: string, value: string) {
    this.#options.envs[key] = value;
    return this;
  }

  cwd(path: string) {
    this.#options.cwd = new Path(path);
    return this;
  }

  build() {
    const command = new Deno.Command(this.#options.command, {
      args: this.#options.args,
      clearEnv: true,
      env: this.#options.envs,
      cwd: this.#options.cwd.toString(),
      stderr: "piped",
      stdout: "piped",
    });
    return new TestCommand(command.spawn());
  }
}

export class TestCommand implements Disposable {
  #child: Deno.ChildProcess;

  constructor(child: Deno.ChildProcess) {
    this.#child = child;
  }

  [Symbol.dispose]() {
    try {
      this.#child.kill();
    } catch {
      // ignore
    }
  }
}

export class TempDir implements Disposable {
  #path: Path;

  constructor(opts?: Deno.MakeTempOptions) {
    this.#path = new Path(Deno.makeTempDirSync(opts));
  }

  get path() {
    return this.#path;
  }

  cleanup() {
    try {
      this.#path.removeSync({ recursive: true });
    } catch (err) {
      if (!(err instanceof Deno.errors.NotFound)) {
        console.warn(
          "Failed cleaning up temp dir",
          this.#path,
          "- Error:",
          err,
        );
      }
    }
  }

  [Symbol.dispose]() {
    this.cleanup();
  }
}
