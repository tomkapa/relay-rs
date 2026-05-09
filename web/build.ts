import tailwind from "bun-plugin-tailwind";

const result = await Bun.build({
  entrypoints: ["./index.html"],
  outdir: "./dist",
  minify: true,
  plugins: [tailwind],
});

if (!result.success) {
  console.error(result.logs);
  process.exit(1);
}

for (const o of result.outputs) {
  console.log(`  ${o.path.replace(import.meta.dir + "/", "")}`);
}
