// Transcribes a 16 kHz mono WAV with whisper.cpp on this PC and writes word timings as JSON.
// Usage: node scripts/whisper.mjs <whisper dir> <input.wav> <output.json>
import { downloadWhisperModel, installWhisperCpp, toCaptions, transcribe } from "@remotion/install-whisper-cpp";
import fs from "node:fs";
import path from "node:path";

const [dir, input, output] = process.argv.slice(2);
const version = "1.5.5";
const model = "base.en";
const say = (o) => console.log(JSON.stringify(o));

say({ step: "Getting Whisper (once, about 150 MB)" });
await installWhisperCpp({ to: dir, version, printOutput: false });
// A download that stops half way leaves a broken model file behind, so it is removed and fetched again next time.
const modelFile = path.join(dir, `ggml-${model}.bin`);
try {
  await downloadWhisperModel({ model, folder: dir, printOutput: false, onProgress: (d, t) => say({ step: "Downloading the Whisper model (once)", pct: t ? (d / t) * 100 : 0 }) });
} catch (e) {
  fs.rmSync(modelFile, { force: true });
  console.error("MODEL_DOWNLOAD_FAILED " + String(e?.message || e));
  process.exit(4);
}
say({ step: "Listening", pct: 0 });
let out;
try {
  out = await transcribe({ inputPath: input, whisperPath: dir, whisperCppVersion: version, model, tokenLevelTimestamps: true, printOutput: false, onProgress: (p) => say({ step: "Listening", pct: p * 100 }) });
} catch (e) {
  if (/bad magic|failed to load model/.test(String(e?.message || e))) fs.rmSync(modelFile, { force: true });
  console.error("TRANSCRIBE_FAILED " + String(e?.message || e).slice(0, 400));
  process.exit(5);
}
const { captions } = toCaptions({ whisperCppOutput: out });
fs.writeFileSync(output, JSON.stringify(captions.map((c) => ({ text: c.text, start: c.startMs / 1000, end: c.endMs / 1000 }))));
say({ step: "Done", pct: 100 });
