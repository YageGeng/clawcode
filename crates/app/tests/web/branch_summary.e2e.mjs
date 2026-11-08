import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { parseArgs } from 'node:util';

import { Page, openBlankPage, startChrome } from './cdp_page.mjs';

// Runs the production app with the repository's claw.toml and its configured
// provider; only the Session data directory and the project are throwaway.
//
//   node crates/app/tests/web/branch_summary.e2e.mjs <with-summary|without-summary>
//     [--model provider/model] [--port 3973]
//
// Two prompts build a branch; navigating back to the first prompt from the
// Session Tree offers pi's summary choice. The real model writes the summary,
// so the script asserts only what a user can observe: the choice panel and
// whether a branch-summary card appears, live and after a reload.

const { values: options, positionals: [mode] } = parseArgs({
  allowPositionals: true,
  options: {
    model: { type: 'string' },
    port: { type: 'string', default: '3973' }
  }
});
assert(['with-summary', 'without-summary'].includes(mode), `unknown mode: ${mode}`);
const base = `http://127.0.0.1:${options.port}/`;
const repo = resolve(import.meta.dirname, '../../../..');
const binary = join(repo, 'target/debug/clawcode');
const COMPOSER = 'textarea[aria-label="消息"]';
const SUMMARY_CARD = '.compaction-card[data-transcript-key^="branchSummary:"]';
const MODEL_WAIT_MS = 3 * 60 * 1000;

const work = await mkdtemp(join(tmpdir(), 'clawcode-branch-e2e-'));
const project = join(work, 'project');
const children = [];

/** Uses the repository config, replacing only the active model when asked. */
async function configPath() {
  const source = join(repo, 'claw.toml');
  if (options.model === undefined) return source;
  const config = (await readFile(source, 'utf8'))
    .replace(/^active_model\s*=.*$/m, `active_model = ${JSON.stringify(options.model)}`);
  const path = join(work, 'config.toml');
  await writeFile(path, config);
  return path;
}

/** Waits until the production server answers its bootstrap route. */
async function waitForServer() {
  for (let attempt = 0; attempt < 100; attempt++) {
    try {
      const response = await fetch(new URL('/api/ui/bootstrap', base));
      if (response.ok) return;
    } catch { /* The server is still starting. */ }
    await delay(200);
  }
  throw new Error('server did not start');
}

/** Opens the WebUI on a new Session in the project. */
async function openPage() {
  const profile = join(work, 'chrome');
  children.push(startChrome(profile));
  const page = await openBlankPage(profile, Page);
  await page.call('Page.navigate', { url: base });
  await page.wait(`!![...document.querySelectorAll('button')].find(item=>item.textContent.trim()==='新建'&&!item.disabled)`);
  await page.click('新建');
  await page.fill('#session-cwd', project);
  await page.click('创建');
  await page.wait(`!!document.querySelector(${JSON.stringify(COMPOSER)})&&!document.querySelector('#session-cwd')`);
  return page;
}

/** Sends one prompt and waits until its Run settles with a usage row. */
async function prompt(page, text, settledRuns) {
  await page.fill(COMPOSER, text);
  await page.click('发送');
  await page.wait(`document.querySelectorAll('.agent-usage').length>=${settledRuns}&&!document.querySelector('.danger-button')`, MODEL_WAIT_MS);
}

/** Opens the Tree inspector and selects the first persisted message entry. */
async function selectFirstMessage(page) {
  await page.evaluate(`(() => { const toggle=document.querySelector('button[aria-label="展开详情栏"]'); if(toggle) toggle.click(); })()`);
  await page.wait(`[...document.querySelectorAll('.tree-entry')].some(item=>item.querySelector('.tree-entry__kind')?.textContent==='message')`);
  await page.evaluate(`[...document.querySelectorAll('.tree-entry')].find(item=>item.querySelector('.tree-entry__kind')?.textContent==='message').click()`);
}

/** Navigates back from the Tree, answering the summary choice. */
async function navigateBack(page) {
  await page.click('Navigate');
  await page.wait(`!!document.querySelector('.tree-summary-choice')`);
  if (mode === 'with-summary') {
    await page.click('带指令生成摘要');
    await page.fill('textarea[aria-label="摘要指令"]', 'keep it short');
    await page.click('确认');
  } else {
    await page.click('不生成摘要');
  }
  await page.wait(`!document.querySelector('.tree-summary-choice')&&!document.querySelector('.operation-status')`, MODEL_WAIT_MS);
}

let failed = false;
try {
  mkdirSync(project, { recursive: true });
  const config = await configPath();
  children.push(spawn(binary, ['serve', '--bind', `127.0.0.1:${options.port}`, '--web-root', join(repo, 'web/dist')], {
    cwd: repo,
    env: { ...process.env, CLAW_CONFIG: config, CLAW_SESSION_PERSISTENCE__DATA_HOME: join(work, 'data') },
    stdio: 'ignore'
  }));
  await waitForServer();
  const page = await openPage();
  await prompt(page, 'Reply with the single word ONE.', 1);
  await prompt(page, 'Reply with the single word TWO.', 2);
  await selectFirstMessage(page);
  await navigateBack(page);
  const expected = mode === 'with-summary';
  await page.wait(`(${JSON.stringify(expected)}) === !!document.querySelector(${JSON.stringify(SUMMARY_CARD)})`, 30_000);
  // A full replay rebuilds the transcript from the store.
  await page.call('Page.reload');
  await page.wait(`!!document.querySelector(${JSON.stringify(COMPOSER)})`);
  await delay(2_000);
  assert.equal(await page.evaluate(`!!document.querySelector(${JSON.stringify(SUMMARY_CARD)})`), expected, 'replay shows the same branch-summary card state');
  assert.deepEqual(page.errors, [], 'the page threw no exceptions');
  console.log(`branch summary e2e passed: ${mode}`);
} catch (error) {
  failed = true;
  console.error(`branch summary e2e failed: ${mode}\n${error.stack ?? error}`);
} finally {
  for (const child of children) child.kill();
  await delay(300);
  await rm(work, { recursive: true, force: true, maxRetries: 3 });
}
process.exit(failed ? 1 : 0);
