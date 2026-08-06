import { cp, mkdir } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import webpack from 'webpack';
import config from '../webpack.ui.config.mjs';

const root = path.dirname(fileURLToPath(new URL('../package.json', import.meta.url)));
const publicDir = path.join(root, 'ui', 'public');
const outputDir = path.join(root, 'dist', 'ui');
const watch = process.argv.includes('--watch');

async function copyPublicAssets() {
  await mkdir(outputDir, { recursive: true });
  await cp(publicDir, outputDir, { recursive: true, force: true });
}

function report(error, stats) {
  if (error) throw error;
  if (!stats) throw new Error('Webpack returned no build statistics.');
  const output = stats.toString({ colors: process.stdout.isTTY, chunks: false, modules: false });
  if (stats.hasErrors()) throw new Error(output);
  if (output.trim()) console.log(output);
}

const compiler = webpack(config);
if (watch) {
  compiler.watch({}, async (error, stats) => {
    try {
      report(error, stats);
      await copyPublicAssets();
      console.log('React management UI rebuilt.');
    } catch (buildError) {
      console.error(buildError);
    }
  });
} else {
  await new Promise((resolve, reject) => {
    compiler.run(async (error, stats) => {
      try {
        report(error, stats);
        await copyPublicAssets();
        compiler.close(closeError => closeError ? reject(closeError) : resolve());
      } catch (buildError) {
        compiler.close(() => reject(buildError));
      }
    });
  });
}
