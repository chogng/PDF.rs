# Valid mixed Reference pixel oracle

## Independent O1 semantic authority

The self-authored page combines a gray path fill, one non-interpolated DeviceRGB image,
and one embedded triangle glyph. Independent operator and resource inspection fixes the
Native Scene command order as `Fill`, `Save`, `DrawImage`, `Restore`, `DrawGlyphRun`.
It also fixes the image's literal red/blue source components and the glyph's black
painting color. This O1 review establishes the case-wide scene and source-order
semantics, but it deliberately does not derive the final 8 by 8 RGBA bytes.

## Reviewed O3 pixel authority

`expected/pixel.json` is an exact implementation-bound O3 regression golden generated
by the frozen Reference renderer. Its decoded raw RGBA bytes have SHA-256
`05c2256f5ef14fc8c0733f273a2827846bf0b854bbaec5027e0278ca7f864a1e`; the
canonical pixel artifact has SHA-256
`f5a08df588fd4fa06d55c02334c53b021ff446c2f4c01a7accc692883e0b89d5`.
Neither digest is presented as an independent O0/O1 pixel derivation.

The frozen implementation is commit
`8c3e28c8ce4cbe5113cc565a36744158e283a7fb`, whose repository tree is
`724c2a646114a8aff0fabe29f6008a8b73802783`. The
`implementation_sha256` in `evidence/reference-identity.json` is reproducibly
defined as SHA-256 over the exact stdout bytes, including each terminating LF, of:

```text
git ls-tree -r --full-tree 8c3e28c8ce4cbe5113cc565a36744158e283a7fb -- pdf-rs/raster
```

That byte stream hashes to
`0088e35c0824ab38b7e2ba41ff56c89d9bf246b611e968cee19cc36475327f5b`.
The hash-bound `evidence/review.json` records independent `spec-conformance` and
`parser-security` review of the derivation, exact pixel reference, and frozen identity.
