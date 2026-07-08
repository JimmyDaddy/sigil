#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";

const repoRoot = path.resolve(import.meta.dirname, "..");
const logoDir = path.join(repoRoot, "assets", "logo");
const markPath = path.join(logoDir, "sigil-mark-transparent.png");
const wordmarkPath = path.join(logoDir, "sigil-wordmark-transparent.png");
const headerWordmarkPath = path.join(logoDir, "sigil-wordmark-header.png");
const outPath = path.join(logoDir, "sigil-full.png");
const onWhitePath = path.join(logoDir, "sigil-full-on-white.png");

const layout = {
  width: 1094,
  height: 545,
  mark: { x: 44, y: 9 },
  wordmark: { x: 475, y: 115 }
};

const pngSignature = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);

function crc32(buffer) {
  let crc = 0xffffffff;
  for (const byte of buffer) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) {
      crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function paeth(left, up, upperLeft) {
  const estimate = left + up - upperLeft;
  const leftDistance = Math.abs(estimate - left);
  const upDistance = Math.abs(estimate - up);
  const upperLeftDistance = Math.abs(estimate - upperLeft);
  if (leftDistance <= upDistance && leftDistance <= upperLeftDistance) {
    return left;
  }
  if (upDistance <= upperLeftDistance) {
    return up;
  }
  return upperLeft;
}

function readPng(filePath) {
  const file = fs.readFileSync(filePath);
  if (!file.subarray(0, 8).equals(pngSignature)) {
    throw new Error(`${filePath} is not a PNG`);
  }

  let offset = 8;
  let width = 0;
  let height = 0;
  let bitDepth = 0;
  let colorType = 0;
  let interlace = 0;
  const idatChunks = [];

  while (offset < file.length) {
    const length = file.readUInt32BE(offset);
    const type = file.subarray(offset + 4, offset + 8).toString("ascii");
    const data = file.subarray(offset + 8, offset + 8 + length);
    offset += 12 + length;

    if (type === "IHDR") {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      bitDepth = data[8];
      colorType = data[9];
      interlace = data[12];
    } else if (type === "IDAT") {
      idatChunks.push(data);
    } else if (type === "IEND") {
      break;
    }
  }

  if (bitDepth !== 8 || colorType !== 6 || interlace !== 0) {
    throw new Error(`${filePath} must be a non-interlaced 8-bit RGBA PNG`);
  }

  const bytesPerPixel = 4;
  const stride = width * bytesPerPixel;
  const inflated = zlib.inflateSync(Buffer.concat(idatChunks));
  const pixels = Buffer.alloc(width * height * bytesPerPixel);
  let sourceOffset = 0;

  for (let y = 0; y < height; y += 1) {
    const filter = inflated[sourceOffset];
    sourceOffset += 1;
    const row = inflated.subarray(sourceOffset, sourceOffset + stride);
    sourceOffset += stride;

    const outRow = pixels.subarray(y * stride, (y + 1) * stride);
    const previousRow = y > 0 ? pixels.subarray((y - 1) * stride, y * stride) : null;

    for (let x = 0; x < stride; x += 1) {
      const raw = row[x];
      const left = x >= bytesPerPixel ? outRow[x - bytesPerPixel] : 0;
      const up = previousRow ? previousRow[x] : 0;
      const upperLeft = previousRow && x >= bytesPerPixel ? previousRow[x - bytesPerPixel] : 0;

      if (filter === 0) {
        outRow[x] = raw;
      } else if (filter === 1) {
        outRow[x] = (raw + left) & 0xff;
      } else if (filter === 2) {
        outRow[x] = (raw + up) & 0xff;
      } else if (filter === 3) {
        outRow[x] = (raw + Math.floor((left + up) / 2)) & 0xff;
      } else if (filter === 4) {
        outRow[x] = (raw + paeth(left, up, upperLeft)) & 0xff;
      } else {
        throw new Error(`unsupported PNG filter ${filter} in ${filePath}`);
      }
    }
  }

  return { width, height, pixels };
}

function lightness(r, g, b) {
  return (r + g + b) / 3;
}

function saturation(r, g, b) {
  const rn = r / 255;
  const gn = g / 255;
  const bn = b / 255;
  const max = Math.max(rn, gn, bn);
  const min = Math.min(rn, gn, bn);
  return max === 0 ? 0 : (max - min) / max;
}

function cleanWhiteHalo(image, options = {}) {
  const {
    haloLumaThreshold = 210,
    haloSaturationThreshold = 0.35,
    neighborLumaThreshold = 225,
    neighborSaturationThreshold = 0.45
  } = options;

  const { width, height, pixels } = image;
  const out = Buffer.from(pixels);

  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const idx = (y * width + x) * 4;
      const alpha = pixels[idx + 3];
      if (alpha === 0) {
        continue;
      }

      const red = pixels[idx];
      const green = pixels[idx + 1];
      const blue = pixels[idx + 2];
      const luma = lightness(red, green, blue);
      const sat = saturation(red, green, blue);
      if (luma <= haloLumaThreshold || sat >= haloSaturationThreshold) {
        continue;
      }

      let hasTransparentOrSemiNeighbor = false;
      let hasSemiNeighbor = false;
      let weightedRed = 0;
      let weightedGreen = 0;
      let weightedBlue = 0;
      let weightedTotal = 0;

      for (let ny = Math.max(0, y - 1); ny <= Math.min(height - 1, y + 1); ny += 1) {
        for (let nx = Math.max(0, x - 1); nx <= Math.min(width - 1, x + 1); nx += 1) {
          if (nx === x && ny === y) {
            continue;
          }
          const nIdx = (ny * width + nx) * 4;
          const nAlpha = pixels[nIdx + 3];
          if (nAlpha === 0) {
            hasTransparentOrSemiNeighbor = true;
            continue;
          }

          const nRed = pixels[nIdx];
          const nGreen = pixels[nIdx + 1];
          const nBlue = pixels[nIdx + 2];
          const nLuma = lightness(nRed, nGreen, nBlue);
          const nSat = saturation(nRed, nGreen, nBlue);
          if (nLuma < neighborLumaThreshold || nSat < neighborSaturationThreshold || nAlpha < 255) {
            weightedRed += nRed * nAlpha;
            weightedGreen += nGreen * nAlpha;
            weightedBlue += nBlue * nAlpha;
            weightedTotal += nAlpha;
            if (nAlpha < 255) {
              hasSemiNeighbor = true;
            }
          }
        }
      }

      if (!hasTransparentOrSemiNeighbor && !hasSemiNeighbor) {
        continue;
      }

      if (weightedTotal === 0) {
        out[idx + 3] = 0;
        continue;
      }

      out[idx] = Math.round(weightedRed / weightedTotal);
      out[idx + 1] = Math.round(weightedGreen / weightedTotal);
      out[idx + 2] = Math.round(weightedBlue / weightedTotal);
      out[idx + 3] = alpha === 255 ? 240 : Math.max(8, Math.round(alpha * 0.86));
    }
  }

  return { width, height, pixels: out };
}

function clearBrightEdgeHalos(image, options = {}) {
  const {
    edgeLumaThreshold = 200,
    edgeSaturationThreshold = 0.6
  } = options;

  const { width, height, pixels } = image;
  const out = Buffer.from(pixels);

  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const idx = (y * width + x) * 4;
      const alpha = out[idx + 3];
      if (alpha === 0) {
        continue;
      }

      let hasTransparentNeighbor = false;
      for (let ny = Math.max(0, y - 1); ny <= Math.min(height - 1, y + 1); ny += 1) {
        for (let nx = Math.max(0, x - 1); nx <= Math.min(width - 1, x + 1); nx += 1) {
          if (nx === x && ny === y) {
            continue;
          }
          if (pixels[(ny * width + nx) * 4 + 3] < 255) {
            hasTransparentNeighbor = true;
            break;
          }
        }
        if (hasTransparentNeighbor) {
          break;
        }
      }

      if (!hasTransparentNeighbor) {
        continue;
      }

      const red = out[idx];
      const green = out[idx + 1];
      const blue = out[idx + 2];
      const luma = lightness(red, green, blue);
      const sat = saturation(red, green, blue);
      if (luma <= edgeLumaThreshold || sat >= edgeSaturationThreshold) {
        continue;
      }

      out[idx + 3] = 0;
    }
  }

  return { width, height, pixels: out };
}

function alphaBounds(image) {
  let minX = image.width;
  let minY = image.height;
  let maxX = -1;
  let maxY = -1;
  for (let y = 0; y < image.height; y += 1) {
    for (let x = 0; x < image.width; x += 1) {
      const alpha = image.pixels[(y * image.width + x) * 4 + 3];
      if (alpha === 0) {
        continue;
      }
      minX = Math.min(minX, x);
      minY = Math.min(minY, y);
      maxX = Math.max(maxX, x);
      maxY = Math.max(maxY, y);
    }
  }
  return maxX === -1 ? null : { minX, minY, maxX, maxY };
}

function cropToAlpha(image, padding = 2) {
  const bounds = alphaBounds(image);
  if (!bounds) {
    throw new Error("cannot crop an image with no visible pixels");
  }

  const minX = Math.max(0, bounds.minX - padding);
  const minY = Math.max(0, bounds.minY - padding);
  const maxX = Math.min(image.width - 1, bounds.maxX + padding);
  const maxY = Math.min(image.height - 1, bounds.maxY + padding);
  const width = maxX - minX + 1;
  const height = maxY - minY + 1;
  const pixels = Buffer.alloc(width * height * 4);

  for (let y = 0; y < height; y += 1) {
    const sourceStart = ((minY + y) * image.width + minX) * 4;
    const sourceEnd = sourceStart + width * 4;
    image.pixels.copy(pixels, y * width * 4, sourceStart, sourceEnd);
  }

  return { width, height, pixels, bounds: { minX, minY, maxX, maxY } };
}

function alphaBlend(canvas, width, source, dx, dy) {
  for (let y = 0; y < source.height; y += 1) {
    const targetY = dy + y;
    if (targetY < 0) {
      continue;
    }
    for (let x = 0; x < source.width; x += 1) {
      const targetX = dx + x;
      if (targetX < 0 || targetX >= width) {
        continue;
      }

      const sourceIndex = (y * source.width + x) * 4;
      const targetIndex = (targetY * width + targetX) * 4;
      const sourceAlpha = source.pixels[sourceIndex + 3] / 255;
      if (sourceAlpha === 0) {
        continue;
      }
      const targetAlpha = canvas[targetIndex + 3] / 255;
      const outAlpha = sourceAlpha + targetAlpha * (1 - sourceAlpha);

      for (let channel = 0; channel < 3; channel += 1) {
        const sourceValue = source.pixels[sourceIndex + channel];
        const targetValue = canvas[targetIndex + channel];
        canvas[targetIndex + channel] = Math.round(
          (sourceValue * sourceAlpha + targetValue * targetAlpha * (1 - sourceAlpha)) / outAlpha
        );
      }
      canvas[targetIndex + 3] = Math.round(outAlpha * 255);
    }
  }
}

function pngChunk(type, data = Buffer.alloc(0)) {
  const typeBuffer = Buffer.from(type, "ascii");
  const crcInput = Buffer.concat([typeBuffer, data]);
  const chunk = Buffer.alloc(12 + data.length);
  chunk.writeUInt32BE(data.length, 0);
  typeBuffer.copy(chunk, 4);
  data.copy(chunk, 8);
  chunk.writeUInt32BE(crc32(crcInput), 8 + data.length);
  return chunk;
}

function writePng(filePath, width, height, pixels) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  ihdr[10] = 0;
  ihdr[11] = 0;
  ihdr[12] = 0;

  const stride = width * 4;
  const raw = Buffer.alloc((stride + 1) * height);
  for (let y = 0; y < height; y += 1) {
    const rowStart = y * (stride + 1);
    raw[rowStart] = 0;
    pixels.copy(raw, rowStart + 1, y * stride, (y + 1) * stride);
  }

  const idat = zlib.deflateSync(raw, { level: 9 });
  const png = Buffer.concat([
    pngSignature,
    pngChunk("IHDR", ihdr),
    pngChunk("IDAT", idat),
    pngChunk("IEND")
  ]);
  fs.writeFileSync(filePath, png);
}

const mark = clearBrightEdgeHalos(cleanWhiteHalo(readPng(markPath)));
const wordmark = clearBrightEdgeHalos(cleanWhiteHalo(readPng(wordmarkPath)));

writePng(markPath, mark.width, mark.height, mark.pixels);
writePng(wordmarkPath, wordmark.width, wordmark.height, wordmark.pixels);

if (process.argv.includes("--info")) {
  const headerWordmark = cropToAlpha(wordmark);
  console.log(JSON.stringify({
    mark: { width: mark.width, height: mark.height, bounds: alphaBounds(mark) },
    wordmark: { width: wordmark.width, height: wordmark.height, bounds: alphaBounds(wordmark) },
    headerWordmark: {
      width: headerWordmark.width,
      height: headerWordmark.height,
      bounds: headerWordmark.bounds
    },
    layout
  }, null, 2));
  process.exit(0);
}

const canvas = Buffer.alloc(layout.width * layout.height * 4);
alphaBlend(canvas, layout.width, mark, layout.mark.x, layout.mark.y);
alphaBlend(canvas, layout.width, wordmark, layout.wordmark.x, layout.wordmark.y);
writePng(outPath, layout.width, layout.height, canvas);
const whiteCanvas = Buffer.alloc(layout.width * layout.height * 4, 255);
alphaBlend(whiteCanvas, layout.width, { width: layout.width, height: layout.height, pixels: canvas }, 0, 0);
writePng(onWhitePath, layout.width, layout.height, whiteCanvas);
const headerWordmark = cropToAlpha(wordmark);
writePng(headerWordmarkPath, headerWordmark.width, headerWordmark.height, headerWordmark.pixels);
console.log(`generated ${path.relative(repoRoot, outPath)}`);
console.log(`generated ${path.relative(repoRoot, onWhitePath)}`);
console.log(`generated ${path.relative(repoRoot, headerWordmarkPath)}`);
