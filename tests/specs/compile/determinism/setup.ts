// for setup, we create two directories with the same file in each
// and then when compiling we ensure this directory name has no
// effect on the output
Deno.mkdirSync("a");
Deno.copyFileSync("main.ts", `a/main.ts`);
