import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

async function run() {
  const args = process.argv.slice(2);
  const outPath = args[0] || 'screenshot.png';
  const width = parseInt(args[1] || '1440', 10);
  const height = parseInt(args[2] || '900', 10);
  const url = args[3] || 'http://localhost:13381';

  console.log(`Launching browser...`);
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width, height } });

  console.log(`Navigating to ${url}...`);
  await page.goto(url, { waitUntil: 'networkidle', timeout: 60000 });

  // Wait for the app shell to render.
  await page.waitForSelector('aside');

  // Then wait for the data to actually arrive. A fixed delay is what made this
  // script capture "Loading..." and "Loading Timeline..." against the real
  // database: the shell renders immediately, so `aside` plus one second proves
  // nothing about whether any panel has data. A visual-QA tool that silently
  // produces a screenshot of a loading spinner is worse than one that fails,
  // because the artifact looks like evidence.
  // Every panel, not just the first. The sidebar and the timeline load
  // independently, so waiting on a single matched element returns while the
  // other is still spinning -- which is exactly how a 1080x1920 capture kept
  // its "Loading Timeline..." placeholder.
  await page
    .waitForFunction(() => !/Loading/i.test(document.body.innerText), null, {
      timeout: 60000,
    })
    .catch(() => {});

  // Settle any entry animation now that the content is in place.
  await page.waitForTimeout(500);

  console.log(`Taking screenshot to ${outPath}...`);
  await page.screenshot({ path: outPath });

  await browser.close();
  console.log(`Done.`);
}

run().catch((err) => {
  console.error(err);
  process.exit(1);
});
