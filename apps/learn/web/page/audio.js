function measure(node, power, length) {
  node.power += power;
  node.measured += length;
  if (node.measured < sampleRate * 0.05) return;
  node.port.postMessage(Math.sqrt(node.power / node.measured));
  node.power = node.measured = 0;
}

class Capture extends AudioWorkletProcessor {
  constructor() {
    super();
    this.batch = new Int16Array(1024);
    this.filled = 0;
    this.power = this.measured = 0;
  }

  process([input]) {
    const samples = input[0] ?? [];
    measure(this, samples.reduce((sum, sample) => sum + sample * sample, 0), samples.length);
    for (const sample of samples) {
      this.batch[this.filled++] = Math.max(-1, Math.min(1, sample)) * 32767;
      if (this.filled === this.batch.length) {
        this.port.postMessage(this.batch.buffer, [this.batch.buffer]);
        this.batch = new Int16Array(1024);
        this.filled = 0;
      }
    }
    return true;
  }
}

class Play extends AudioWorkletProcessor {
  constructor() {
    super();
    this.ring = new Float32Array(sampleRate * 2);
    this.read = 0;
    this.write = 0;
    this.waiting = true;
    this.loud = false;
    this.quiet = 0;
    this.power = this.measured = 0;
    this.port.onmessage = ({ data }) => this.take(new Int16Array(data));
  }

  take(samples) {
    for (const sample of samples) {
      const next = (this.write + 1) % this.ring.length;
      if (next === this.read) return;
      this.ring[this.write] = sample / 32768;
      this.write = next;
    }
  }

  process(inputs, [output]) {
    const out = output[0];
    const held = (this.write - this.read + this.ring.length) % this.ring.length;
    if (this.waiting && held < sampleRate * 0.06) return this.level(0, out.length);
    this.waiting = false;
    let power = 0;
    for (let i = 0; i < out.length; i++) {
      if (this.read === this.write) {
        this.waiting = true;
        break;
      }
      out[i] = this.ring[this.read];
      power += out[i] * out[i];
      this.read = (this.read + 1) % this.ring.length;
    }
    return this.level(power, out.length);
  }

  level(power, length) {
    measure(this, power, length);
    const loud = Math.sqrt(power / length) > 0.02;
    this.quiet = loud ? 0 : this.quiet + length;
    if (loud !== this.loud && (loud || this.quiet > sampleRate * 0.3)) {
      this.loud = loud;
      this.port.postMessage(loud);
    }
    return true;
  }
}

registerProcessor("capture", Capture);
registerProcessor("play", Play);
