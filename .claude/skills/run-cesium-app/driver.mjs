#!/usr/bin/env node
// Headless Chromium driver for cesium-app, controlled via stdin commands
// (one per line). No tmux in this environment, so it's driven by piping a
// heredoc of commands to this process's stdin — see SKILL.md.
//
// Commands:
//   goto [url]              navigate (default http://localhost:5173/)
//   wait <ms>                sleep
//   screenshot <path>        save a PNG
//   eval <js expression>     evaluate in page context, prints EVAL_RESULT <json>
//   click <selector>         CSS selector click
//   console                  dump captured console/page-error messages so far
//   quit | exit              close the browser and end the process
//
// Each command prints exactly one line starting with OK/ERR/EVAL_RESULT/
// CONSOLE_DUMP when done, so a caller reading stdout line-by-line can tell
// when a command has finished before sending the next one.

import { chromium } from 'playwright';
import readline from 'node:readline';

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });

const consoleMessages = [];
page.on('console', (msg) => consoleMessages.push(`[${msg.type()}] ${msg.text()}`));
page.on('pageerror', (err) => consoleMessages.push(`[pageerror] ${err.message}`));

const rl = readline.createInterface({ input: process.stdin });

for await (const line of rl) {
  const trimmed = line.trim();
  if (!trimmed) continue;
  const spaceIdx = trimmed.indexOf(' ');
  const cmd = spaceIdx === -1 ? trimmed : trimmed.slice(0, spaceIdx);
  const arg = spaceIdx === -1 ? '' : trimmed.slice(spaceIdx + 1);

  try {
    switch (cmd) {
      case 'goto':
        await page.goto(arg || 'http://localhost:5173/', { waitUntil: 'load' });
        console.log(`OK goto ${arg}`);
        break;
      case 'wait':
        await page.waitForTimeout(parseInt(arg, 10) || 1000);
        console.log(`OK wait ${arg}`);
        break;
      case 'screenshot':
        await page.screenshot({ path: arg || '/tmp/cesium-app-screenshot.png' });
        console.log(`OK screenshot ${arg}`);
        break;
      case 'eval': {
        const result = await page.evaluate(arg);
        console.log(`EVAL_RESULT ${JSON.stringify(result)}`);
        break;
      }
      case 'click':
        await page.click(arg);
        console.log(`OK click ${arg}`);
        break;
      case 'console':
        console.log(`CONSOLE_DUMP ${JSON.stringify(consoleMessages)}`);
        break;
      case 'quit':
      case 'exit':
        await browser.close();
        process.exit(0);
        break;
      default:
        console.log(`ERR unknown command: ${cmd}`);
    }
  } catch (e) {
    console.log(`ERR ${cmd}: ${e.message}`);
  }
}

await browser.close();
