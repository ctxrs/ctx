export class CtxError extends Error {
  constructor(message, options = {}) {
    super(message, options.cause ? { cause: options.cause } : undefined);
    this.name = "CtxError";
    this.code = options.code ?? "CTX_ERROR";
    this.details = options.details;
    this.retryable = options.retryable ?? false;
  }
}

export class CtxCliError extends CtxError {
  constructor(message, options = {}) {
    super(message, {
      code: options.code ?? "CTX_CLI_ERROR",
      retryable: options.retryable,
      details: {
        command: options.command,
        args: options.args,
        exitCode: options.exitCode,
        signal: options.signal,
        stdout: options.stdout,
        stderr: options.stderr,
        ...options.details,
      },
      cause: options.cause,
    });
    this.name = "CtxCliError";
    this.exitCode = options.exitCode;
    this.signal = options.signal;
    this.stdout = options.stdout ?? "";
    this.stderr = options.stderr ?? "";
    this.command = options.command;
    this.args = options.args ?? [];
  }
}

export class CtxParseError extends CtxError {
  constructor(message, options = {}) {
    super(message, {
      code: options.code ?? "CTX_PARSE_ERROR",
      details: options.details,
      cause: options.cause,
    });
    this.name = "CtxParseError";
  }
}

export class CtxValidationError extends CtxError {
  constructor(message, options = {}) {
    super(message, {
      code: options.code ?? "CTX_VALIDATION_ERROR",
      details: options.details,
      cause: options.cause,
    });
    this.name = "CtxValidationError";
  }
}

export class CtxUnsupportedError extends CtxError {
  constructor(message, options = {}) {
    super(message, {
      code: options.code ?? "CTX_UNSUPPORTED",
      details: options.details,
      cause: options.cause,
    });
    this.name = "CtxUnsupportedError";
  }
}

export class CtxTimeoutError extends CtxError {
  constructor(message, options = {}) {
    super(message, {
      code: options.code ?? "timeout",
      retryable: true,
      details: options.details,
      cause: options.cause,
    });
    this.name = "CtxTimeoutError";
  }
}
