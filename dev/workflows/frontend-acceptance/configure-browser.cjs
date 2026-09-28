// Explicit setup only: resolve the installed task-local browser; never download or start it.
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require('playwright');
const configPath = path.join(__dirname, 'browser.json');
const executablePath = chromium.executablePath();
if (!fs.statSync(executablePath).isFile()) {
  throw new Error('installed Chromium executable is unavailable');
}
const config = JSON.parse(fs.readFileSync(configPath, 'utf8'));
config.browser.launchOptions.executablePath = executablePath;
fs.writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`);
process.stdout.write('Configured the installed Chromium executable for this workflow.\n');
