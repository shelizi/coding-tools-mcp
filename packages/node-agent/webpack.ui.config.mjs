import path from 'node:path';
import { fileURLToPath } from 'node:url';
import MiniCssExtractPlugin from 'mini-css-extract-plugin';

const root = path.dirname(fileURLToPath(import.meta.url));

export default {
  mode: 'production',
  target: ['web', 'es2022'],
  entry: path.join(root, 'ui', 'src', 'main.tsx'),
  output: {
    path: path.join(root, 'dist', 'ui'),
    filename: 'app.js',
    clean: {
      keep: /^(manifest\.webmanifest|icon\.svg|sw\.js)$/
    }
  },
  resolve: {
    extensions: ['.tsx', '.ts', '.js']
  },
  module: {
    rules: [
      {
        test: /\.tsx?$/,
        exclude: /node_modules/,
        use: {
          loader: 'ts-loader',
          options: {
            configFile: path.join(root, 'tsconfig.ui.json')
          }
        }
      },
      {
        test: /\.css$/i,
        use: [MiniCssExtractPlugin.loader, 'css-loader']
      }
    ]
  },
  plugins: [
    new MiniCssExtractPlugin({ filename: 'app.css' })
  ],
  optimization: {
    runtimeChunk: false,
    splitChunks: false
  },
  performance: {
    hints: false
  },
  devtool: false,
  stats: 'errors-warnings'
};
