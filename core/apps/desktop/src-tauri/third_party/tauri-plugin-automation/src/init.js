function resolve(id, result) {
  __TAURI_INTERNALS__.invoke("plugin:automation|resolve", {
    id,
    result:
      result instanceof Error
        ? {
            error: result.name,
            message: result.message,
            stacktrace: result.stack,
          }
        : result,
  });
}

Object.defineProperty(window, "__AUTOMATION__", {
  value: {
    resolve,
  },
});
