exports.globalThis = globalThis;
exports.global = global;
exports.window = window ?? globalThis.window ?? global.window;
