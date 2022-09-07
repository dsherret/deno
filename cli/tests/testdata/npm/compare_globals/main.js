import * as cjsGlobals from "npm:@denotest/cjs-globals";
import * as esmGlobals from "npm:@denotest/esm-globals";
console.log(cjsGlobals.global === cjsGlobals.globalThis);
console.log(cjsGlobals.window);
console.log(esmGlobals.global === esmGlobals.globalThis);
console.log(esmGlobals.window);
