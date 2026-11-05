import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

const DEFAULT_CHROME = process.platform === 'darwin'
  ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
  : '/opt/google/chrome/chrome';

/** Owns one CDP connection and correlates only this script's requests. */
export class Page {
  pending = new Map();
  errors = [];
  sequence = 0;
  constructor(socket) {
    this.socket = socket;
    socket.addEventListener('message', ({ data }) => {
      const frame = JSON.parse(data);
      if (frame.method === 'Runtime.exceptionThrown') this.errors.push(frame.params.exceptionDetails.text);
      const waiter = this.pending.get(frame.id);
      if (!waiter) return;
      this.pending.delete(frame.id);
      if (frame.error) waiter.reject(new Error(JSON.stringify(frame.error)));
      else waiter.resolve(frame.result);
    });
  }
  /** Sends a CDP operation without relying on an additional browser framework. */
  call(method, params = {}) {
    return new Promise((resolve, reject) => {
      const id = ++this.sequence;
      this.pending.set(id, { resolve, reject });
      this.socket.send(JSON.stringify({ id, method, params }));
    });
  }
  /** Evaluates an observable page action and propagates page exceptions. */
  async evaluate(expression) {
    const result = await this.call('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
    if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
    return result.result.value;
  }
  /** Waits for a visible or protocol condition within a bounded acceptance window. */
  async wait(expression, timeout = 30_000) {
    const until = Date.now() + timeout;
    while (Date.now() < until) {
      const value = await this.evaluate(expression);
      if (value) return value;
      await delay(100);
    }
    throw new Error(`Timed out: ${expression}`);
  }
  /** Uses the native value setter so the real React input handler receives the edit. */
  async fill(selector, value) {
    await this.evaluate(`(() => { const element=document.querySelector(${JSON.stringify(selector)}); if(!element)throw Error('missing input'); const prototype=element.tagName==='TEXTAREA'?HTMLTextAreaElement.prototype:HTMLInputElement.prototype; Object.getOwnPropertyDescriptor(prototype,'value').set.call(element,${JSON.stringify(value)}); element.dispatchEvent(new Event('input',{bubbles:true})); })()`);
  }
  /** Clicks a visible enabled button by its exact displayed text. */
  async click(text) {
    await this.evaluate(`(() => {const button=[...document.querySelectorAll('button')].find(item=>item.textContent.trim()===${JSON.stringify(text)}&&!item.disabled);if(!button)throw Error('missing button '+${JSON.stringify(text)});button.click()})()`);
  }
}

/** Starts headless Chrome on a throwaway profile; `CHROME_BIN` overrides the binary. */
export function startChrome(profile) {
  return spawn(process.env.CHROME_BIN ?? DEFAULT_CHROME, [
    '--headless=new', '--no-sandbox', '--disable-gpu', '--no-first-run', '--disable-dev-shm-usage',
    '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank'
  ], { stdio: ['ignore', 'ignore', 'ignore'] });
}

/** Connects to the Chrome running on `profile` and opens one blank page. */
export async function openBlankPage(profile, PageClass = Page) {
  let port;
  for (let attempt = 0; attempt < 100; attempt++) {
    try { port = Number((await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0]); break; }
    catch { await delay(100); }
  }
  assert(port, 'Chrome started');
  const target = await (await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, { method: 'PUT' })).json();
  const socket = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { socket.addEventListener('open', resolve, { once: true }); socket.addEventListener('error', reject, { once: true }); });
  const page = new PageClass(socket);
  await page.call('Runtime.enable');
  await page.call('Page.enable');
  return page;
}
