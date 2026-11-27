// Block-compressed texels a diagnostics run saved as raw blocks
// (frame-<n>-<i>-<resource>-<w>x<h>-f<format>-s<slice>.bin, VICTORY_DUMP_PED_TEXTURES=1)
// turned into PNG pictures beside them, to see what a texture held: a colour map, a normal map,
// or another texture's bytes.
// usage: node bc-png.mjs <file.bin | folder> [more...]
// BC1-BC5 and BC7 are decoded. BC7 blocks of three subsets (modes 0 and 2) are drawn with the
// two-subset partition of the same number: their colours are right, edges inside a block may
// not be. BC5 is drawn as a normal map (x, y, and z rebuilt). Alpha is dropped.
import fs from 'node:fs';
import path from 'node:path';
import zlib from 'node:zlib';

const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
});
function crc(buf) {
    let c = 0xffffffff;
    for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
}
function chunk(type, data) {
    const head = Buffer.alloc(8);
    head.writeUInt32BE(data.length, 0);
    head.write(type, 4, 'ascii');
    const tail = Buffer.alloc(4);
    tail.writeUInt32BE(crc(Buffer.concat([head.subarray(4), data])), 0);
    return Buffer.concat([head, data, tail]);
}
function png(w, h, rgb) {
    const raw = Buffer.alloc((w * 3 + 1) * h);
    for (let y = 0; y < h; y++) rgb.copy(raw, y * (w * 3 + 1) + 1, y * w * 3, (y + 1) * w * 3);
    const ihdr = Buffer.alloc(13);
    ihdr.writeUInt32BE(w, 0);
    ihdr.writeUInt32BE(h, 4);
    ihdr[8] = 8;
    ihdr[9] = 2;
    return Buffer.concat([
        Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
        chunk('IHDR', ihdr),
        chunk('IDAT', zlib.deflateSync(raw)),
        chunk('IEND', Buffer.alloc(0)),
    ]);
}

// 16 texels of a block, each [r, g, b].
function bc1(b, o, alwaysFour) {
    const c0 = b.readUInt16LE(o), c1 = b.readUInt16LE(o + 2);
    const e = (c) => [((c >> 11) & 31) * 255 / 31, ((c >> 5) & 63) * 255 / 63, (c & 31) * 255 / 31];
    const a = e(c0), z = e(c1);
    const mix = (p, q, t) => p.map((v, i) => v + (q[i] - v) * t);
    const pal = c0 > c1 || alwaysFour
        ? [a, z, mix(a, z, 1 / 3), mix(a, z, 2 / 3)]
        : [a, z, mix(a, z, 1 / 2), [0, 0, 0]];
    const bits = b.readUInt32LE(o + 4);
    return Array.from({ length: 16 }, (_, i) => pal[(bits >>> (2 * i)) & 3]);
}
function bc4(b, o) {
    const r0 = b[o], r1 = b[o + 1];
    const pal = [r0, r1];
    if (r0 > r1) for (let i = 1; i < 7; i++) pal.push(((7 - i) * r0 + i * r1) / 7);
    else {
        for (let i = 1; i < 5; i++) pal.push(((5 - i) * r0 + i * r1) / 5);
        pal.push(0, 255);
    }
    let bits = 0n;
    for (let i = 0; i < 6; i++) bits |= BigInt(b[o + 2 + i]) << BigInt(8 * i);
    return Array.from({ length: 16 }, (_, i) => pal[Number((bits >> BigInt(3 * i)) & 7n)]);
}

const P2 = [
    0xcccc, 0x8888, 0xeeee, 0xecc8, 0xc880, 0xfeec, 0xfec8, 0xec80, 0xc800, 0xffec, 0xfe80, 0xe800, 0xffe8, 0xff00, 0xfff0, 0xf000,
    0xf710, 0x008e, 0x7100, 0x08ce, 0x008c, 0x7310, 0x3100, 0x8cce, 0x088c, 0x3110, 0x6666, 0x366c, 0x17e8, 0x0ff0, 0x718e, 0x399c,
    0xaaaa, 0xf0f0, 0x5a5a, 0x33cc, 0x3c3c, 0x55aa, 0x9696, 0xa55a, 0x73ce, 0x13c8, 0x324c, 0x3bdc, 0x6996, 0xc33c, 0x9966, 0x0660,
    0x0272, 0x04e4, 0x4e40, 0x2720, 0xc936, 0x936c, 0x39c6, 0x639c, 0x9336, 0x9cc6, 0x817e, 0xe718, 0xccf0, 0x0fcc, 0x7744, 0xee22,
];
const A2 = [
    15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 2, 8, 2, 2, 8, 8, 15, 2, 8, 2, 2, 8, 8, 2, 2,
    15, 15, 6, 8, 2, 8, 15, 15, 2, 8, 2, 2, 2, 15, 15, 6, 6, 2, 6, 8, 15, 15, 2, 2, 15, 15, 15, 15, 15, 2, 2, 15,
];
const A3a = [
    3, 3, 15, 15, 8, 3, 15, 15, 8, 8, 6, 6, 6, 5, 3, 3, 3, 3, 8, 15, 3, 3, 6, 10, 5, 8, 8, 6, 8, 5, 15, 15,
    8, 15, 3, 5, 6, 10, 8, 15, 15, 3, 15, 5, 15, 15, 15, 15, 3, 15, 5, 5, 5, 8, 5, 10, 5, 10, 8, 13, 15, 12, 3, 3,
];
const A3b = [
    15, 8, 8, 3, 15, 15, 3, 8, 15, 15, 15, 15, 15, 15, 15, 8, 15, 8, 15, 3, 15, 8, 15, 8, 3, 15, 6, 10, 15, 15, 10, 8,
    15, 3, 15, 10, 10, 8, 9, 10, 6, 15, 8, 15, 3, 6, 6, 8, 15, 3, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 3, 15, 15, 8,
];
const W = {
    2: [0, 21, 43, 64],
    3: [0, 9, 18, 27, 37, 46, 55, 64],
    4: [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64],
};
// subsets, partition bits, rotation bits, index-selection bits, colour bits, alpha bits,
// endpoint p-bits, shared p-bits, index bits, second index bits
const MODES = [
    [3, 4, 0, 0, 4, 0, 1, 0, 3, 0],
    [2, 6, 0, 0, 6, 0, 0, 1, 3, 0],
    [3, 6, 0, 0, 5, 0, 0, 0, 2, 0],
    [2, 6, 0, 0, 7, 0, 1, 0, 2, 0],
    [1, 0, 2, 1, 5, 6, 0, 0, 2, 3],
    [1, 0, 2, 0, 7, 8, 0, 0, 2, 2],
    [1, 0, 0, 0, 7, 7, 1, 0, 4, 0],
    [2, 6, 0, 0, 5, 5, 1, 0, 2, 0],
];
function bc7(b, o) {
    let bits = 0n;
    for (let i = 0; i < 16; i++) bits |= BigInt(b[o + i]) << BigInt(8 * i);
    let at = 0;
    const take = (n) => {
        const v = Number((bits >> BigInt(at)) & ((1n << BigInt(n)) - 1n));
        at += n;
        return v;
    };
    let mode = 0;
    while (mode < 8 && take(1) === 0) mode++;
    if (mode === 8) return Array.from({ length: 16 }, () => [0, 0, 0]);
    const [ns, pb, rb, isb, cb, ab, epb, spb, ib, ib2] = MODES[mode];
    const part = take(pb), rot = take(rb), sel = take(isb);
    const ends = ns * 2;
    const col = [[], [], []];
    for (let c = 0; c < 3; c++) for (let e = 0; e < ends; e++) col[c].push(take(cb));
    const alpha = [];
    for (let e = 0; e < ends; e++) alpha.push(ab ? take(ab) : 255);
    const pbits = [];
    if (epb) for (let e = 0; e < ends; e++) pbits.push(take(1));
    if (spb) for (let s = 0; s < ns; s++) {
        const p = take(1);
        pbits.push(p, p);
    }
    const expand = (v, n) => {
        v <<= 8 - n;
        return v | (v >> n);
    };
    const end = [];
    for (let e = 0; e < ends; e++) {
        const p = pbits.length ? pbits[e] : null;
        const c = [0, 1, 2].map((k) => (p === null ? expand(col[k][e], cb) : expand((col[k][e] << 1) | p, cb + 1)));
        const a = ab ? (p === null ? expand(alpha[e], ab) : expand((alpha[e] << 1) | p, ab + 1)) : 255;
        end.push([...c, a]);
    }
    const subset = (i) => (ns === 1 ? 0 : (P2[part] >> i) & 1) + (ns === 3 && i > 11 && ((P2[part] >> i) & 1) === 0 ? 2 : 0);
    const anchors = ns === 1 ? [0] : ns === 2 ? [0, A2[part]] : [0, A3a[part], A3b[part]];
    const idx = [], idx2 = [];
    for (let i = 0; i < 16; i++) idx.push(take(anchors.includes(i) ? ib - 1 : ib));
    if (ib2) for (let i = 0; i < 16; i++) idx2.push(take(i === 0 ? ib2 - 1 : ib2));
    const lerp = (p, q, w) => (p * (64 - w) + q * w + 32) >> 6;
    return Array.from({ length: 16 }, (_, i) => {
        const s = Math.min(subset(i), ns - 1);
        const [p, q] = [end[2 * s], end[2 * s + 1]];
        let [ci, ai, cbits, abits] = [idx[i], idx[i], ib, ib];
        if (ib2) {
            [ci, ai, cbits, abits] = sel ? [idx2[i], idx[i], ib2, ib] : [idx[i], idx2[i], ib, ib2];
        }
        const c = [0, 1, 2].map((k) => lerp(p[k], q[k], W[cbits][ci]));
        const a = lerp(p[3], q[3], W[abits][ai]);
        const t = [...c, a];
        if (rot) [t[rot - 1], t[3]] = [t[3], t[rot - 1]];
        return t.slice(0, 3);
    });
}

function decode(file) {
    const m = /-(\d+)x(\d+)-f(\d+)-s\d+\.bin$/.exec(file);
    if (!m) return;
    const [w, h, f] = [+m[1], +m[2], +m[3]];
    const kind = f >= 70 && f <= 72 ? 'bc1' : f <= 75 ? 'bc2' : f <= 78 ? 'bc3' : f <= 81 ? 'bc4' : f <= 84 ? 'bc5'
        : f >= 97 && f <= 99 ? 'bc7' : null;
    if (!kind) {
        console.log(`${file}: format ${f} not decoded`);
        return;
    }
    const data = fs.readFileSync(file);
    const bw = Math.ceil(w / 4), bh = Math.ceil(h / 4);
    const size = kind === 'bc1' || kind === 'bc4' ? 8 : 16;
    if (data.length < bw * bh * size) {
        console.log(`${file}: ${data.length} bytes, ${bw * bh * size} expected`);
        return;
    }
    const rgb = Buffer.alloc(w * h * 3);
    let sum = [0, 0, 0];
    for (let by = 0; by < bh; by++) for (let bx = 0; bx < bw; bx++) {
        const o = (by * bw + bx) * size;
        let t;
        if (kind === 'bc1') t = bc1(data, o, false);
        else if (kind === 'bc2' || kind === 'bc3') t = bc1(data, o + 8, true);
        else if (kind === 'bc4') t = bc4(data, o).map((v) => [v, v, v]);
        else if (kind === 'bc5') {
            const x = bc4(data, o), y = bc4(data, o + 8);
            t = x.map((xv, i) => {
                const nx = xv / 127.5 - 1, ny = y[i] / 127.5 - 1;
                const nz = Math.sqrt(Math.max(0, 1 - nx * nx - ny * ny));
                return [xv, y[i], (nz + 1) * 127.5];
            });
        } else t = bc7(data, o);
        for (let i = 0; i < 16; i++) {
            const x = bx * 4 + (i & 3), y = by * 4 + (i >> 2);
            if (x >= w || y >= h) continue;
            for (let k = 0; k < 3; k++) {
                const v = Math.max(0, Math.min(255, Math.round(t[i][k])));
                rgb[(y * w + x) * 3 + k] = v;
                sum[k] += v;
            }
        }
    }
    const out = file.replace(/\.bin$/, '.png');
    fs.writeFileSync(out, png(w, h, rgb));
    const mean = sum.map((s) => (s / (w * h) / 255).toFixed(2)).join(', ');
    console.log(`${out}: ${kind}, mean colour ${mean}`);
}

for (const arg of process.argv.slice(2)) {
    const files = fs.statSync(arg).isDirectory()
        ? fs.readdirSync(arg).filter((n) => /-f\d+-s\d+\.bin$/.test(n)).map((n) => path.join(arg, n))
        : [arg];
    for (const f of files) decode(f);
}
