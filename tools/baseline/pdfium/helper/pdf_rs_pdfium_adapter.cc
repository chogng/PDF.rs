#include <algorithm>
#include <array>
#include <bit>
#include <climits>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <memory>
#include <new>
#include <optional>
#include <span>

#if defined(_WIN32)
#include <fcntl.h>
#include <io.h>
#endif

#include "public/fpdf_edit.h"
#include "public/fpdfview.h"

namespace {

constexpr std::array<uint8_t, 8> kRequestMagic = {'P', 'R', 'S', 'B',
                                                  'R', 'E', 'Q', '2'};
constexpr std::array<uint8_t, 8> kResponseMagic = {'P', 'R', 'S', 'B',
                                                   'O', 'B', 'S', '2'};
constexpr uint16_t kSchemaVersion = 2;
constexpr size_t kRequestHeaderBytes = 96;
constexpr size_t kResponseHeaderBytes = 112;
constexpr uint64_t kMaxPdfBytes = 64ULL * 1024ULL * 1024ULL;
constexpr uint64_t kMaxRgbaBytes = 64ULL * 1024ULL * 1024ULL;
constexpr int kRenderFlags = FPDF_ANNOT;

constexpr std::array<uint32_t, 64> kSha256RoundConstants = {
    0x428a2f98U, 0x71374491U, 0xb5c0fbcfU, 0xe9b5dba5U, 0x3956c25bU,
    0x59f111f1U, 0x923f82a4U, 0xab1c5ed5U, 0xd807aa98U, 0x12835b01U,
    0x243185beU, 0x550c7dc3U, 0x72be5d74U, 0x80deb1feU, 0x9bdc06a7U,
    0xc19bf174U, 0xe49b69c1U, 0xefbe4786U, 0x0fc19dc6U, 0x240ca1ccU,
    0x2de92c6fU, 0x4a7484aaU, 0x5cb0a9dcU, 0x76f988daU, 0x983e5152U,
    0xa831c66dU, 0xb00327c8U, 0xbf597fc7U, 0xc6e00bf3U, 0xd5a79147U,
    0x06ca6351U, 0x14292967U, 0x27b70a85U, 0x2e1b2138U, 0x4d2c6dfcU,
    0x53380d13U, 0x650a7354U, 0x766a0abbU, 0x81c2c92eU, 0x92722c85U,
    0xa2bfe8a1U, 0xa81a664bU, 0xc24b8b70U, 0xc76c51a3U, 0xd192e819U,
    0xd6990624U, 0xf40e3585U, 0x106aa070U, 0x19a4c116U, 0x1e376c08U,
    0x2748774cU, 0x34b0bcb5U, 0x391c0cb3U, 0x4ed8aa4aU, 0x5b9cca4fU,
    0x682e6ff3U, 0x748f82eeU, 0x78a5636fU, 0x84c87814U, 0x8cc70208U,
    0x90befffaU, 0xa4506cebU, 0xbef9a3f7U, 0xc67178f2U,
};

class Sha256 final {
 public:
  Sha256() = default;

  void Update(std::span<const uint8_t> input) {
    total_bytes_ += static_cast<uint64_t>(input.size());
    while (!input.empty()) {
      const size_t amount =
          std::min(input.size(), block_.size() - block_size_);
      std::span<uint8_t> destination(block_);
      std::ranges::copy(input.first(amount),
                        destination.subspan(block_size_, amount).begin());
      block_size_ += amount;
      input = input.subspan(amount);
      if (block_size_ == block_.size()) {
        Transform(block_);
        block_size_ = 0;
      }
    }
  }

  std::array<uint8_t, 32> Final() {
    const uint64_t bit_length = total_bytes_ * 8ULL;
    block_[block_size_++] = 0x80U;
    if (block_size_ > 56U) {
      std::fill(block_.begin() + static_cast<ptrdiff_t>(block_size_),
                block_.end(), 0U);
      Transform(block_);
      block_size_ = 0;
    }
    std::fill(block_.begin() + static_cast<ptrdiff_t>(block_size_),
              block_.begin() + 56, 0U);
    for (size_t index = 0; index < 8U; ++index) {
      const unsigned int shift =
          static_cast<unsigned int>((7U - index) * 8U);
      block_[56U + index] =
          static_cast<uint8_t>((bit_length >> shift) & 0xffU);
    }
    Transform(block_);

    std::array<uint8_t, 32> output = {};
    for (size_t index = 0; index < state_.size(); ++index) {
      const uint32_t value = state_[index];
      output[index * 4U] = static_cast<uint8_t>(value >> 24U);
      output[index * 4U + 1U] = static_cast<uint8_t>(value >> 16U);
      output[index * 4U + 2U] = static_cast<uint8_t>(value >> 8U);
      output[index * 4U + 3U] = static_cast<uint8_t>(value);
    }
    return output;
  }

 private:
  void Transform(std::span<const uint8_t, 64> block) {
    std::array<uint32_t, 64> words = {};
    for (size_t index = 0; index < 16U; ++index) {
      const size_t offset = index * 4U;
      words[index] = (static_cast<uint32_t>(block[offset]) << 24U) |
                     (static_cast<uint32_t>(block[offset + 1U]) << 16U) |
                     (static_cast<uint32_t>(block[offset + 2U]) << 8U) |
                     static_cast<uint32_t>(block[offset + 3U]);
    }
    for (size_t index = 16U; index < words.size(); ++index) {
      const uint32_t left = words[index - 15U];
      const uint32_t right = words[index - 2U];
      const uint32_t sigma0 =
          std::rotr(left, 7) ^ std::rotr(left, 18) ^ (left >> 3U);
      const uint32_t sigma1 =
          std::rotr(right, 17) ^ std::rotr(right, 19) ^ (right >> 10U);
      words[index] = words[index - 16U] + sigma0 + words[index - 7U] + sigma1;
    }

    uint32_t a = state_[0];
    uint32_t b = state_[1];
    uint32_t c = state_[2];
    uint32_t d = state_[3];
    uint32_t e = state_[4];
    uint32_t f = state_[5];
    uint32_t g = state_[6];
    uint32_t h = state_[7];
    for (size_t index = 0; index < words.size(); ++index) {
      const uint32_t sigma1 =
          std::rotr(e, 6) ^ std::rotr(e, 11) ^ std::rotr(e, 25);
      const uint32_t choose = (e & f) ^ ((~e) & g);
      const uint32_t temporary1 =
          h + sigma1 + choose + kSha256RoundConstants[index] + words[index];
      const uint32_t sigma0 =
          std::rotr(a, 2) ^ std::rotr(a, 13) ^ std::rotr(a, 22);
      const uint32_t majority = (a & b) ^ (a & c) ^ (b & c);
      const uint32_t temporary2 = sigma0 + majority;
      h = g;
      g = f;
      f = e;
      e = d + temporary1;
      d = c;
      c = b;
      b = a;
      a = temporary1 + temporary2;
    }
    state_[0] += a;
    state_[1] += b;
    state_[2] += c;
    state_[3] += d;
    state_[4] += e;
    state_[5] += f;
    state_[6] += g;
    state_[7] += h;
  }

  std::array<uint32_t, 8> state_ = {
      0x6a09e667U, 0xbb67ae85U, 0x3c6ef372U, 0xa54ff53aU,
      0x510e527fU, 0x9b05688cU, 0x1f83d9abU, 0x5be0cd19U,
  };
  std::array<uint8_t, 64> block_ = {};
  size_t block_size_ = 0;
  uint64_t total_bytes_ = 0;
};

struct Request final {
  uint32_t page;
  uint32_t width;
  uint32_t height;
  std::array<uint8_t, 32> source_hash;
  std::array<uint8_t, 32> descriptor_identity;
  std::unique_ptr<uint8_t[]> pdf;
  size_t pdf_size;
};

enum class RenderStatus {
  kSuccess,
  kPixelFailure,
  kTerminalFailure,
};

struct RenderResult final {
  RenderStatus status;
  std::unique_ptr<uint8_t[]> rgba;
  size_t rgba_size;
};

class PdfiumLibrary final {
 public:
  PdfiumLibrary() {
    FPDF_LIBRARY_CONFIG config = {};
    config.version = 6;
    config.m_pUserFontPaths = font_paths_.data();
    config.m_pIsolate = nullptr;
    config.m_v8EmbedderSlot = 0;
    config.m_pPlatform = nullptr;
    config.m_RendererType = FPDF_RENDERERTYPE_AGG;
    config.m_FontLibraryType = FPDF_FONTBACKENDTYPE_FREETYPE;
    config.m_BrotliEnabled = false;
    FPDF_InitLibraryWithConfig(&config);
    FPDF_SetSandBoxPolicy(FPDF_POLICY_MACHINETIME_ACCESS, false);
  }

  PdfiumLibrary(const PdfiumLibrary&) = delete;
  PdfiumLibrary& operator=(const PdfiumLibrary&) = delete;

  ~PdfiumLibrary() { FPDF_DestroyLibrary(); }

 private:
  std::array<const char*, 1> font_paths_ = {nullptr};
};

class ScopedDocument final {
 public:
  explicit ScopedDocument(FPDF_DOCUMENT document) : document_(document) {}
  ScopedDocument(const ScopedDocument&) = delete;
  ScopedDocument& operator=(const ScopedDocument&) = delete;
  ~ScopedDocument() {
    if (document_) {
      FPDF_CloseDocument(document_);
    }
  }
  FPDF_DOCUMENT get() const { return document_; }

 private:
  FPDF_DOCUMENT document_;
};

class ScopedPage final {
 public:
  explicit ScopedPage(FPDF_PAGE page) : page_(page) {}
  ScopedPage(const ScopedPage&) = delete;
  ScopedPage& operator=(const ScopedPage&) = delete;
  ~ScopedPage() {
    if (page_) {
      FPDF_ClosePage(page_);
    }
  }
  FPDF_PAGE get() const { return page_; }

 private:
  FPDF_PAGE page_;
};

class ScopedBitmap final {
 public:
  explicit ScopedBitmap(FPDF_BITMAP bitmap) : bitmap_(bitmap) {}
  ScopedBitmap(const ScopedBitmap&) = delete;
  ScopedBitmap& operator=(const ScopedBitmap&) = delete;
  ~ScopedBitmap() {
    if (bitmap_) {
      FPDFBitmap_Destroy(bitmap_);
    }
  }
  FPDF_BITMAP get() const { return bitmap_; }

 private:
  FPDF_BITMAP bitmap_;
};

uint16_t ReadU16(const std::array<uint8_t, kRequestHeaderBytes>& bytes,
                 size_t offset) {
  return static_cast<uint16_t>(
      (static_cast<uint16_t>(bytes[offset]) << 8U) |
      static_cast<uint16_t>(bytes[offset + 1U]));
}

uint32_t ReadU32(const std::array<uint8_t, kRequestHeaderBytes>& bytes,
                 size_t offset) {
  return (static_cast<uint32_t>(bytes[offset]) << 24U) |
         (static_cast<uint32_t>(bytes[offset + 1U]) << 16U) |
         (static_cast<uint32_t>(bytes[offset + 2U]) << 8U) |
         static_cast<uint32_t>(bytes[offset + 3U]);
}

uint64_t ReadU64(const std::array<uint8_t, kRequestHeaderBytes>& bytes,
                 size_t offset) {
  uint64_t value = 0;
  for (size_t index = 0; index < 8U; ++index) {
    value = (value << 8U) | static_cast<uint64_t>(bytes[offset + index]);
  }
  return value;
}

template <size_t Size>
void WriteU16(std::array<uint8_t, Size>* bytes,
              size_t offset,
              uint16_t value) {
  (*bytes)[offset] = static_cast<uint8_t>(value >> 8U);
  (*bytes)[offset + 1U] = static_cast<uint8_t>(value);
}

template <size_t Size>
void WriteU32(std::array<uint8_t, Size>* bytes,
              size_t offset,
              uint32_t value) {
  (*bytes)[offset] = static_cast<uint8_t>(value >> 24U);
  (*bytes)[offset + 1U] = static_cast<uint8_t>(value >> 16U);
  (*bytes)[offset + 2U] = static_cast<uint8_t>(value >> 8U);
  (*bytes)[offset + 3U] = static_cast<uint8_t>(value);
}

template <size_t Size>
void WriteU64(std::array<uint8_t, Size>* bytes,
              size_t offset,
              uint64_t value) {
  for (size_t index = 0; index < 8U; ++index) {
    const unsigned int shift =
        static_cast<unsigned int>((7U - index) * 8U);
    (*bytes)[offset + index] =
        static_cast<uint8_t>((value >> shift) & 0xffU);
  }
}

bool ReadExact(FILE* input, std::span<uint8_t> output) {
  while (!output.empty()) {
    const size_t amount =
        std::fread(output.data(), sizeof(uint8_t), output.size(), input);
    if (amount == 0U) {
      return false;
    }
    output = output.subspan(amount);
  }
  return true;
}

bool WriteExact(FILE* output, std::span<const uint8_t> input) {
  while (!input.empty()) {
    const size_t amount =
        std::fwrite(input.data(), sizeof(uint8_t), input.size(), output);
    if (amount == 0U) {
      return false;
    }
    input = input.subspan(amount);
  }
  return true;
}

std::optional<Request> ReadRequest() {
  std::array<uint8_t, kRequestHeaderBytes> header = {};
  if (!ReadExact(stdin, header) ||
      !std::equal(kRequestMagic.begin(), kRequestMagic.end(), header.begin()) ||
      ReadU16(header, 8U) != kSchemaVersion || ReadU16(header, 10U) != 0U) {
    return std::nullopt;
  }

  const uint32_t page = ReadU32(header, 12U);
  const uint32_t width = ReadU32(header, 16U);
  const uint32_t height = ReadU32(header, 20U);
  const uint64_t pdf_size_u64 = ReadU64(header, 24U);
  if (width == 0U || height == 0U || pdf_size_u64 > kMaxPdfBytes) {
    return std::nullopt;
  }
  const size_t pdf_size = static_cast<size_t>(pdf_size_u64);
  const size_t allocation_size = std::max<size_t>(pdf_size, 1U);
  std::unique_ptr<uint8_t[]> pdf(
      new (std::nothrow) uint8_t[allocation_size]);
  if (!pdf || !ReadExact(stdin, std::span<uint8_t>(pdf.get(), pdf_size))) {
    return std::nullopt;
  }
  const int trailing = std::fgetc(stdin);
  if (trailing != EOF || std::ferror(stdin) != 0) {
    return std::nullopt;
  }

  std::array<uint8_t, 32> source_hash = {};
  std::array<uint8_t, 32> descriptor_identity = {};
  for (size_t index = 0; index < source_hash.size(); ++index) {
    source_hash[index] = header[32U + index];
    descriptor_identity[index] = header[64U + index];
  }
  Sha256 hasher;
  hasher.Update(std::span<const uint8_t>(pdf.get(), pdf_size));
  if (hasher.Final() != source_hash) {
    return std::nullopt;
  }

  return Request{page,        width,          height, source_hash,
                 descriptor_identity, std::move(pdf), pdf_size};
}

RenderResult Failure(RenderStatus status) {
  return RenderResult{status, nullptr, 0U};
}

RenderResult Render(const Request& request) {
  const uint64_t rgba_size_u64 = static_cast<uint64_t>(request.width) *
                                 static_cast<uint64_t>(request.height) * 4ULL;
  if (rgba_size_u64 == 0U || rgba_size_u64 > kMaxRgbaBytes ||
      request.width > static_cast<uint32_t>(INT_MAX) ||
      request.height > static_cast<uint32_t>(INT_MAX) ||
      request.width > static_cast<uint32_t>(INT_MAX / 4) ||
      request.page > static_cast<uint32_t>(INT_MAX)) {
    return Failure(RenderStatus::kPixelFailure);
  }

  const int width = static_cast<int>(request.width);
  const int height = static_cast<int>(request.height);
  const int stride = width * 4;
  const size_t rgba_size = static_cast<size_t>(rgba_size_u64);

  PdfiumLibrary library;
  ScopedDocument document(FPDF_LoadMemDocument64(
      request.pdf.get(), request.pdf_size, /*password=*/nullptr));
  if (!document.get()) {
    return Failure(RenderStatus::kTerminalFailure);
  }
  const int page_count = FPDF_GetPageCount(document.get());
  if (page_count <= 0 || request.page >= static_cast<uint32_t>(page_count)) {
    return Failure(RenderStatus::kTerminalFailure);
  }
  ScopedPage page(FPDF_LoadPage(document.get(), static_cast<int>(request.page)));
  if (!page.get()) {
    return Failure(RenderStatus::kTerminalFailure);
  }

  const bool has_transparency = FPDFPage_HasTransparency(page.get()) != 0;
  const int bitmap_format =
      has_transparency ? FPDFBitmap_BGRA : FPDFBitmap_BGRx;
  std::unique_ptr<uint8_t[]> bgra(new (std::nothrow) uint8_t[rgba_size]);
  if (!bgra) {
    return Failure(RenderStatus::kPixelFailure);
  }
  ScopedBitmap bitmap(FPDFBitmap_CreateEx(width, height, bitmap_format,
                                           bgra.get(), stride));
  if (!bitmap.get()) {
    return Failure(RenderStatus::kPixelFailure);
  }
  const FPDF_DWORD fill_color = static_cast<FPDF_DWORD>(
      has_transparency ? 0x00000000UL : 0xFFFFFFFFUL);
  if (FPDFBitmap_FillRect(bitmap.get(), 0, 0, width, height, fill_color) ==
      0) {
    return Failure(RenderStatus::kPixelFailure);
  }
  FPDF_RenderPageBitmap(bitmap.get(), page.get(), 0, 0, width, height,
                        /*rotate=*/0, kRenderFlags);
  if (FPDFBitmap_GetFormat(bitmap.get()) != bitmap_format ||
      FPDFBitmap_GetWidth(bitmap.get()) != width ||
      FPDFBitmap_GetHeight(bitmap.get()) != height ||
      FPDFBitmap_GetStride(bitmap.get()) != stride ||
      FPDFBitmap_GetBuffer(bitmap.get()) != bgra.get()) {
    return Failure(RenderStatus::kPixelFailure);
  }

  std::unique_ptr<uint8_t[]> rgba(new (std::nothrow) uint8_t[rgba_size]);
  if (!rgba) {
    return Failure(RenderStatus::kPixelFailure);
  }
  const std::span<const uint8_t> source(bgra.get(), rgba_size);
  std::span<uint8_t> destination(rgba.get(), rgba_size);
  for (size_t offset = 0; offset < rgba_size; offset += 4U) {
    destination[offset] = source[offset + 2U];
    destination[offset + 1U] = source[offset + 1U];
    destination[offset + 2U] = source[offset];
    destination[offset + 3U] =
        has_transparency ? source[offset + 3U] : uint8_t{255};
  }
  return RenderResult{RenderStatus::kSuccess, std::move(rgba), rgba_size};
}

bool WriteResponse(const Request& request, const RenderResult& result) {
  std::array<uint8_t, kResponseHeaderBytes> header = {};
  std::ranges::copy(kResponseMagic, header.begin());
  WriteU16(&header, 8U, kSchemaVersion);
  switch (result.status) {
    case RenderStatus::kSuccess:
      WriteU16(&header, 10U, 0U);
      header[12] = 1U;
      header[13] = 1U;
      header[14] = 1U;
      header[15] = 0U;
      break;
    case RenderStatus::kPixelFailure:
      WriteU16(&header, 10U, 0U);
      header[12] = 1U;
      header[13] = 1U;
      header[14] = 1U;
      header[15] = 2U;
      break;
    case RenderStatus::kTerminalFailure:
      WriteU16(&header, 10U, 1U);
      header[12] = 2U;
      header[13] = 2U;
      header[14] = 2U;
      header[15] = 2U;
      break;
  }
  WriteU32(&header, 28U, request.page);
  WriteU32(&header, 32U, request.width);
  WriteU32(&header, 36U, request.height);
  const uint64_t rgba_size = result.status == RenderStatus::kSuccess
                                 ? static_cast<uint64_t>(result.rgba_size)
                                 : 0U;
  WriteU64(&header, 40U, rgba_size);
  for (size_t index = 0; index < request.source_hash.size(); ++index) {
    header[48U + index] = request.source_hash[index];
    header[80U + index] = request.descriptor_identity[index];
  }
  if (!WriteExact(stdout, header)) {
    return false;
  }
  if (result.status == RenderStatus::kSuccess &&
      (!result.rgba ||
       !WriteExact(stdout,
                   std::span<const uint8_t>(result.rgba.get(),
                                            result.rgba_size)))) {
    return false;
  }
  return std::fflush(stdout) == 0;
}

bool ConfigureBinaryStdio() {
#if defined(_WIN32)
  return _setmode(_fileno(stdin), _O_BINARY) != -1 &&
         _setmode(_fileno(stdout), _O_BINARY) != -1;
#else
  return true;
#endif
}

}  // namespace

int main(int argc, char*[]) {
  if (argc != 1 || !ConfigureBinaryStdio()) {
    std::fputs("RPE-PDFIUM-ADAPTER-0001\n", stderr);
    return 7;
  }
  std::optional<Request> request = ReadRequest();
  if (!request.has_value()) {
    std::fputs("RPE-PDFIUM-ADAPTER-0001\n", stderr);
    return 7;
  }
  const RenderResult result = Render(*request);
  if (!WriteResponse(*request, result)) {
    std::fputs("RPE-PDFIUM-ADAPTER-0004\n", stderr);
    return 7;
  }
  if (result.status == RenderStatus::kPixelFailure) {
    std::fputs("RPE-PDFIUM-ADAPTER-0003\n", stderr);
  } else if (result.status == RenderStatus::kTerminalFailure) {
    std::fputs("RPE-PDFIUM-ADAPTER-0002\n", stderr);
  }
  return 0;
}
