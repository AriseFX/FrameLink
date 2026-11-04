// Integer limited-range coefficients match the vendored libyuv matrices.
// WIDTH, HEIGHT, STRIDE, YC, UC, VC are specialized once at creation.
Texture2D<float4> image : register(t0);
RWByteAddressBuffer output : register(u0);

int3 rgb(uint x, uint y) {
    return int3(round(image.Load(int3(x, y, 0)).rgb * 255.0));
}
uint luma(int3 p) { return uint(dot(p, YC) + 4224) >> 8; }
uint chroma(int3 a, int3 b, int3 c, int3 d) {
    int3 p = (a + b + c + d + 2) >> 2;
    uint u = uint(dot(p, UC) + 32768) >> 8;
    uint v = uint(dot(p, VC) + 32768) >> 8;
    return u | (v << 8);
}
[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    uint x = id.x * 4, y = id.y * 2;
    if (x >= WIDTH || y >= HEIGHT) return;
    int3 a = rgb(x, y), b = rgb(x + 1, y);
    int3 c = rgb(x, y + 1), d = rgb(x + 1, y + 1);
    uint y0 = luma(a) | (luma(b) << 8);
    uint y1 = luma(c) | (luma(d) << 8);
    uint uv = chroma(a, b, c, d);
    if (x + 2 < WIDTH) {
        a = rgb(x + 2, y); b = rgb(x + 3, y);
        c = rgb(x + 2, y + 1); d = rgb(x + 3, y + 1);
        y0 |= (luma(a) << 16) | (luma(b) << 24);
        y1 |= (luma(c) << 16) | (luma(d) << 24);
        uv |= chroma(a, b, c, d) << 16;
    }
    // One thread owns each DWORD, including the padded right edge.
    output.Store(y * STRIDE + x, y0);
    output.Store((y + 1) * STRIDE + x, y1);
    output.Store(HEIGHT * STRIDE + id.y * STRIDE + x, uv);
}
