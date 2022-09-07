const localWindow = window ?? globalThis.window ?? global.window;
export {
  globalThis,
  global,
  localWindow as window
};
