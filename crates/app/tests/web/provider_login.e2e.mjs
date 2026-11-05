import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { setTimeout as delay } from 'node:timers/promises';
import { parseArgs } from 'node:util';

import { Page, openBlankPage, startChrome } from './cdp_page.mjs';

// Runs the production app and its configured providers; only the credential directory and
// the starting model are isolated, so no real credential is read or changed.
//
//   node crates/app/tests/web/provider_login.e2e.mjs <cancel|blocked-prompt|login|stdio>
//     [--start provider/model] [--login-model provider/model] [--port 3971]
//
// `cancel`, `blocked-prompt`, and `stdio` request one device code and never authorize it.
// `login` needs a person to enter the printed code with a real account.

const { values: options, positionals: [mode] } = parseArgs({
  allowPositionals: true,
  options: {
    start: { type: 'string', default: 'deepseek/deepseek-flash' },
    'login-model': { type: 'string', default: 'chatgpt/gpt-6-astra' },
    port: { type: 'string', default: '3971' }
  }
});
assert(['cancel', 'blocked-prompt', 'login', 'stdio'].includes(mode), `unknown mode: ${mode}`);
const loginModel = options['login-model'];
const methodId = loginModel.split('/')[0];
const startModel = mode === 'blocked-prompt' ? loginModel : options.start;
const base = `http://127.0.0.1:${options.port}/`;
const repo = resolve(import.meta.dirname, '../../../..');
const binary = join(repo, 'target/debug/clawcode');
const COMPOSER = 'textarea[aria-label="消息"]';
const MODEL_SELECT = 'select[aria-label="Model"]';
const SIGN_IN_CODE = '.sign-in-code code';
const MAX_AUTHORIZATION_MS = 15 * 60 * 1000;

const work = await mkdtemp(join(tmpdir(), 'clawcode-login-e2e-'));
const authDir = join(work, 'auth');
const children = [];

/** Isolates only the credential directory and starting model of the repository config. */
async function writeConfig() {
  const source = await readFile(join(repo, 'claw.toml'), 'utf8');
  assert(/\bauth_dir\s*=/.test(source), 'claw.toml has no provider with auth_dir');
  const config = source
    .replace(/\bauth_dir\s*=\s*"[^"]*"/g, `auth_dir = ${JSON.stringify(authDir)}`)
    .replace(/^active_model\s*=.*$/m, `active_model = ${JSON.stringify(startModel)}`);
  const path = join(work, 'config.toml');
  await writeFile(path, config);
  return path;
}

/** Starts one production process against the isolated config and session store. */
function startApp(config, args, stdio) {
  const child = spawn(binary, args, {
    cwd: repo,
    env: { ...process.env, CLAW_CONFIG: config, CLAW_SESSION_PERSISTENCE__DATA_HOME: join(work, 'data') },
    stdio
  });
  children.push(child);
  return child;
}

/** Proves stdio stays pure protocol while the code goes to stderr for clients without URL mode. */
async function stdioMode(config) {
  const app = startApp(config, ['stdio'], ['pipe', 'pipe', 'pipe']);
  const stdout = [];
  const waiters = new Map();
  createInterface({ input: app.stdout }).on('line', (line) => {
    stdout.push(line);
    const frame = JSON.parse(line);
    waiters.get(frame.id)?.(frame);
  });
  let stderr = '';
  app.stderr.on('data', (chunk) => { stderr += chunk; });
  const send = (frame) => app.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', ...frame })}\n`);
  const reply = (id) => new Promise((done) => { waiters.set(id, done); });

  const initialized = reply(1);
  send({ id: 1, method: 'initialize', params: { protocolVersion: 2, info: { name: 'login-e2e', version: '0.0.0' }, capabilities: {} } });
  assert((await initialized).result.authMethods.some((method) => method.methodId === methodId));

  const login = reply(2);
  send({ id: 2, method: 'auth/login', params: { methodId } });
  const deadline = Date.now() + 40_000;
  while (!stderr.includes('sign-in:')) {
    assert(Date.now() < deadline, `no device code on stderr: ${stderr.slice(-500)}`);
    await delay(100);
  }
  send({ method: '$/cancel_request', params: { requestId: 2 } });
  assert.equal((await login).error?.code, -32800);
  assert(!stdout.some((line) => line.includes('elicitation/create')), 'no elicitation for a client without URL mode');
  assert(!stdout.some((line) => line.includes('sign-in:')), 'the code never reaches stdout');
  assert(!existsSync(join(authDir, 'chatgpt.json')), 'an unauthorized sign-in stores nothing');
}

/** Adds the header's model select to the shared page driver. */
class LoginPage extends Page {
  /** Chooses a model the way the header select does. */
  async chooseModel(value) {
    await this.evaluate(`(() => { const select=document.querySelector(${JSON.stringify(MODEL_SELECT)}); if(!select)throw Error('missing model select'); Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype,'value').set.call(select,${JSON.stringify(value)}); select.dispatchEvent(new Event('change',{bubbles:true})); })()`);
  }
  /** Reads the model the header shows. */
  selectedModel() {
    return this.evaluate(`document.querySelector(${JSON.stringify(MODEL_SELECT)})?.value`);
  }
}

/** Opens the production WebUI in headless Chrome on a new Session. */
async function openPage() {
  const profile = join(work, 'chrome');
  children.push(startChrome(profile));
  const page = await openBlankPage(profile, LoginPage);
  await page.call('Page.navigate', { url: base });
  await page.wait(`!![...document.querySelectorAll('button')].find(item=>item.textContent.trim()==='新建'&&!item.disabled)`);
  await page.click('新建');
  await page.fill('#session-cwd', work);
  await page.click('创建');
  await page.wait(`!!document.querySelector(${JSON.stringify(COMPOSER)})&&!document.querySelector('#session-cwd')&&!!document.querySelector(${JSON.stringify(MODEL_SELECT)})`);
  return page;
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

/** Cancelling the sign-in leaves the model and the store untouched. */
async function cancelMode(page) {
  await page.chooseModel(loginModel);
  const code = await page.wait(`document.querySelector(${JSON.stringify(SIGN_IN_CODE)})?.textContent`, 40_000);
  assert(code.length > 0, 'the dialog shows a code');
  assert.equal(await page.evaluate(`document.querySelector(${JSON.stringify(MODEL_SELECT)}).disabled`), true, 'the select waits for the sign-in');
  await page.click('取消');
  await page.wait(`!document.querySelector(${JSON.stringify(SIGN_IN_CODE)})`);
  await page.wait(`!document.querySelector(${JSON.stringify(MODEL_SELECT)}).disabled`);
  assert.equal(await page.selectedModel(), startModel, 'the model did not change');
  assert.equal(await page.evaluate(`!!document.querySelector('.session-config__error')`), false, 'a cancel reports no failure');
  assert(!existsSync(join(authDir, 'chatgpt.json')), 'a cancelled sign-in stores nothing');
}

/** A refused prompt stays out of history and comes back to the composer. */
async function blockedPromptMode(page) {
  const text = 'login e2e: this message must come back';
  await page.fill(COMPOSER, text);
  await page.click('发送');
  await page.wait(`!!document.querySelector(${JSON.stringify(SIGN_IN_CODE)})`, 40_000);
  await page.wait(`document.querySelector(${JSON.stringify(COMPOSER)}).value.includes(${JSON.stringify(text)})`);
  assert.equal(await page.evaluate(`document.querySelectorAll('.message[data-role=user]').length`), 0, 'no user message entered history');
  await page.click('取消');
  await page.wait(`!document.querySelector(${JSON.stringify(SIGN_IN_CODE)})`);
  assert(!existsSync(join(authDir, 'chatgpt.json')), 'a cancelled sign-in stores nothing');
}

/** A person authorizes with a real account; the switch then finishes and a message is answered. */
async function loginMode(page) {
  await page.chooseModel(loginModel);
  const code = await page.wait(`document.querySelector(${JSON.stringify(SIGN_IN_CODE)})?.textContent`, 40_000);
  console.log(`\nOpen https://auth.openai.com/codex/device\nEnter code: ${code}\nWaiting up to 15 minutes…\n`);
  await page.wait(`!document.querySelector(${JSON.stringify(SIGN_IN_CODE)})`, MAX_AUTHORIZATION_MS);
  await page.wait(`document.querySelector(${JSON.stringify(MODEL_SELECT)}).value===${JSON.stringify(loginModel)}`);
  assert(existsSync(join(authDir, 'chatgpt.json')), 'the credential landed in the isolated directory');
  await page.fill(COMPOSER, 'Reply with exactly the word OK. Do not use any tools.');
  await page.click('发送');
  await page.wait(`[...document.querySelectorAll('.message[data-role=assistant] .message__body')].some(node=>node.innerText.trim().length>0)&&!document.querySelector('.danger-button')`, 120_000);
}

let failed = false;
try {
  const config = await writeConfig();
  if (mode === 'stdio') {
    await stdioMode(config);
  } else {
    startApp(config, ['serve', '--bind', `127.0.0.1:${options.port}`, '--web-root', join(repo, 'web/dist')], 'ignore');
    await waitForServer();
    const page = await openPage();
    if (mode === 'cancel') await cancelMode(page);
    else if (mode === 'blocked-prompt') await blockedPromptMode(page);
    else await loginMode(page);
    assert.deepEqual(page.errors, [], 'the page threw no exceptions');
  }
  console.log(`provider login e2e passed: ${mode}`);
} catch (error) {
  failed = true;
  console.error(`provider login e2e failed: ${mode}\n${error.stack ?? error}`);
} finally {
  for (const child of children) child.kill();
  await delay(300);
  await rm(work, { recursive: true, force: true, maxRetries: 3 });
}
process.exit(failed ? 1 : 0);
