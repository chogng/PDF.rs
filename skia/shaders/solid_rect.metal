#include <metal_stdlib>

using namespace metal;

struct SolidVertex {
    float2 position;
};

vertex float4 skia_solid_rect_vertex(
    const device SolidVertex* vertices [[buffer(0)]],
    uint vertex_id [[vertex_id]]) {
    return float4(vertices[vertex_id].position, 0.0, 1.0);
}

fragment float4 skia_solid_rect_fragment(
    constant float4& color [[buffer(0)]]) {
    return color;
}
