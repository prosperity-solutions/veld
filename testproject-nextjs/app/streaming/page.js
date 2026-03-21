import { Suspense } from 'react';

async function SlowData() {
  await new Promise(r => setTimeout(r, 500));
  return <p id="streamed-content">Streamed content loaded</p>;
}

export default function StreamingPage() {
  return (
    <div>
      <h1>Streaming SSR Test</h1>
      <Suspense fallback={<p>Loading...</p>}>
        <SlowData />
      </Suspense>
    </div>
  );
}
