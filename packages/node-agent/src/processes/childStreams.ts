import type { ChildProcessWithoutNullStreams } from 'node:child_process';

export function waitForReadableEnd(stream: ChildProcessWithoutNullStreams['stdout']): Promise<void> {
  if (stream.readableEnded || stream.destroyed) return Promise.resolve();
  return new Promise(resolve => {
    const done = () => {
      stream.off('end', done);
      stream.off('close', done);
      stream.off('error', done);
      resolve();
    };
    stream.once('end', done);
    stream.once('close', done);
    stream.once('error', done);
    if (stream.readableEnded || stream.destroyed) done();
  });
}

export async function waitForChildStreams(child: ChildProcessWithoutNullStreams): Promise<void> {
  await Promise.all([waitForReadableEnd(child.stdout), waitForReadableEnd(child.stderr)]);
}
