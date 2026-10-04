import http from 'k6/http';
import crypto from 'k6/crypto';
import exec from 'k6/execution';
import { check } from 'k6';
import { Trend, Counter } from 'k6/metrics';
import { SharedArray } from 'k6/data';

const scenario = __ENV.WORKLOAD || 'mixed';
const rate = Number(__ENV.RATE || 2000);
const duration = __ENV.DURATION || '30s';
const base = 'http://127.0.0.1:8080';
const key = '0123456789abcdef0123456789abcdef'; // Synthetic, never a production key.
function syntheticText(length) {
  const alphabet = '0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz-_';
  let state = 12345;
  const characters = [];
  for (let index = 0; index < length; index++) {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    characters.push(alphabet[state >>> 26]);
  }
  return characters.join('');
}
const body = new SharedArray('synthetic-body', () => [
  JSON.stringify({ type: 'benchmark', data: syntheticText(Number(__ENV.BODY_BYTES || 1024)) })
])[0];
const trends = Object.fromEntries(['live', 'ready', 'fallback', 'inert_webhook', 'webhook', 'metrics']
  .map(name => [name, new Trend(`latency_${name}`, true)]));
const statuses = new Counter('response_status');
const thresholds = Object.fromEntries(Object.keys(trends).map(name => [`latency_${name}`, ['p(95)<60000']]));

export const options = {
  scenarios: {
    primary: { executor: 'constant-arrival-rate', rate, timeUnit: '1s', duration,
      preAllocatedVUs: Number(__ENV.VUS || 256), maxVUs: Number(__ENV.VUS || 256),
      gracefulStop: '10s' },
    ...(__ENV.SCRAPE === '1' ? { diagnostics: { executor: 'constant-arrival-rate',
      exec: 'scrape', rate: 2, timeUnit: '1s', duration, preAllocatedVUs: 2,
      maxVUs: 2, gracefulStop: '10s' } } : {}),
  },
  thresholds,
  summaryTrendStats: ['avg', 'min', 'med', 'max', 'p(50)', 'p(95)', 'p(99)'],
  discardResponseBodies: true,
};

export function setup() {
  console.info(JSON.stringify({ fixture_bytes: body.length,
    fixture_sha256: crypto.sha256(body, 'hex') }));
}

function send(name, method, path, payload, headers, expected) {
  const response = http.request(method, base + path, payload,
    { headers, tags: { route: name }, timeout: '9s', responseCallback: http.expectedStatuses(expected) });
  trends[name].add(response.timings.duration);
  statuses.add(1, { route: name, status: String(response.status) });
  check(response, { [`${name} expected status ${expected}`]: r => r.status === expected });
}

function webhook(duplicate, overrideId = null) {
  const iteration = Number(__ENV.FIXED_ID_WIDTH || 0) > 0
    ? String(exec.scenario.iterationInTest).padStart(Number(__ENV.FIXED_ID_WIDTH), '0')
    : exec.scenario.iterationInTest;
  const id = overrideId || (duplicate ? 'duplicate-seed' : `${__ENV.RUN_ID}-${iteration}`);
  const timestamp = Math.floor(Date.now() / 1000).toString();
  const signature = crypto.hmac('sha256', key, `${id}.${timestamp}.${body}`, 'base64');
  send('webhook', 'POST', '/webhooks/bench', body, { 'content-type': 'application/json',
    'webhook-id': id, 'webhook-timestamp': timestamp, 'webhook-signature': `v1,${signature}` }, 204);
}

export default function () {
  if (scenario === 'webhook_burst') return webhook(false,
    `${__ENV.RUN_ID}-burst-${Math.floor(exec.scenario.iterationInTest / 32)}`);
  if (scenario === 'webhook_new') return webhook(false);
  if (scenario === 'webhook_duplicate') return webhook(true);
  if (scenario === 'webhook_mix') {
    const index = exec.scenario.iterationInTest % 10;
    if (index < 7) return webhook(false);
    if (index < 9) return webhook(true);
    return send('ready', 'GET', '/health/ready', null, {}, 200);
  }
  if (scenario === 'live') return send('live', 'GET', '/health/live', null, {}, 200);
  if (scenario === 'ready') return send('ready', 'GET', '/health/ready', null, {}, 200);
  const index = exec.scenario.iterationInTest % 10;
  if (index < 4) return send('live', 'GET', '/health/live', null, {}, 200);
  if (index < 8) return send('ready', 'GET', '/health/ready', null, {}, 200);
  if (index === 8) return send('fallback', 'GET', '/missing', null, {}, 404);
  send('inert_webhook', 'POST', '/webhooks/unknown', body, { 'content-type': 'application/json' }, 404);
}

export function scrape() {
  const response = http.get('http://127.0.0.1:9090/metrics', { tags: { route: 'metrics' } });
  trends.metrics.add(response.timings.duration);
  check(response, { 'metrics 200': r => r.status === 200 });
}
