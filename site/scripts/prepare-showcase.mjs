import { cp, mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const source = fileURLToPath(new URL('../../showcase/', import.meta.url));
const output = fileURLToPath(new URL('../public/showcase/', import.meta.url));
const data = JSON.parse(await readFile(path.join(source, 'runs.json'), 'utf8'));
const providers = new Set(['Anthropic', 'OpenAI', 'Moonshot AI']);

for (const key of ['models', 'tasks']) {
    const values = data[key];
    if (
        !Array.isArray(values) ||
        !values.length ||
        new Set(values).size !== values.length ||
        values.some((value) => typeof value !== 'string' || !/^[a-z0-9][a-z0-9.-]*$/.test(value))
    ) {
        throw new Error(`${key} must contain unique, non-empty, valid identifiers`);
    }
}
const expected = new Set(
    data.models.flatMap((model) => data.tasks.map((task) => `${model}/${task}`)),
);
const seen = new Set();
const modelProviders = new Map();
for (const run of data.runs) {
    const key = `${run.model}/${run.task}`;
    if (!expected.has(key) || seen.has(key)) throw new Error(`Unexpected or duplicate run: ${key}`);
    seen.add(key);
    if (!providers.has(run.vendor)) throw new Error(`Unknown provider: ${run.vendor}`);
    if (modelProviders.has(run.model) && modelProviders.get(run.model) !== run.vendor) {
        throw new Error(`Inconsistent provider for ${run.model}`);
    }
    modelProviders.set(run.model, run.vendor);
    for (const [field, directory, extension] of [
        ['gif', 'gifs', 'gif'],
        ['replay', 'replays', 'jsonl'],
    ]) {
        if (run[field] !== `${directory}/${key}.${extension}`)
            throw new Error(`Invalid ${field}: ${key}`);
        if (!(await stat(path.join(source, run[field]))).isFile())
            throw new Error(`Missing ${run[field]}`);
    }
}
if (seen.size !== expected.size) throw new Error('The declared model/brief matrix is incomplete');
for (const task of data.tasks) await readFile(path.join(source, 'tasks', `${task}.txt`), 'utf8');

await mkdir(output, { recursive: true });
for (const directory of ['gifs', 'replays', 'tasks']) {
    const destination = path.join(output, directory);
    await rm(destination, { recursive: true, force: true });
    await cp(path.join(source, directory), destination, { recursive: true });
}
await cp(path.join(source, 'runs.json'), path.join(output, 'runs.json'));
await writeFile(path.join(output, '..', '.nojekyll'), '');
const fontLicenses = await Promise.all(
    ['inter', 'fraunces', 'pixelify-sans'].map(
        async (font) =>
            `${font}\n${await readFile(new URL(`../node_modules/@fontsource-variable/${font}/LICENSE`, import.meta.url), 'utf8')}`,
    ),
);
await writeFile(path.join(output, '..', 'font-licenses.txt'), fontLicenses.join('\n\n'));
console.log(`Prepared ${seen.size} original runs from ${data.models.length} models.`);
