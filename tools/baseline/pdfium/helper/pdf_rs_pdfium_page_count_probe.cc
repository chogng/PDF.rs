#include <algorithm>
#include <array>
#include <bit>
#include <climits>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <optional>
#include <span>
#include <string>
#include <vector>

#if defined(_WIN32)
#include <fcntl.h>
#include <io.h>
#endif

#include "public/fpdfview.h"

namespace {

constexpr std::array<uint8_t, 8> kRequestMagic = {'P', 'R', 'S', 'B',
                                                  'R', 'E', 'Q', '2'};
constexpr std::array<uint8_t, 8> kResponseMagic = {'P', 'R', 'S', 'B',
                                                   'O', 'B', 'S', '2'};
constexpr uint16_t kSchemaVersion = 2;
constexpr size_t kRequestHeaderBytes = 96;
constexpr size_t kResponseHeaderBytes = 112;
constexpr size_t kMaxPdfBytes = 64U * 1024U * 1024U;
constexpr size_t kMaxRequestBytes = kRequestHeaderBytes + kMaxPdfBytes;
constexpr size_t kMaxJsonBytes = 64U;

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
  void Update(std::span<const uint8_t> input) {
    total_bytes_ += static_cast<uint64_t>(input.size());
    while (!input.empty()) {
      const size_t amount = std::min(input.size(), block_.size() - block_size_);
      std::ranges::copy(input.first(amount),
                        block_.begin() + static_cast<ptrdiff_t>(block_size_));
      block_size_ += amount;
      input = input.subspan(amount);
      if (block_size_ == block_.size()) {
        Transform(block_);
        block_size_ = 0;
      }
    }
  }

  std::array<uint8_t, 32> Final() {
    const uint64_t bit_length = total_bytes_ * 8U;
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
      const unsigned int shift = static_cast<unsigned int>((7U - index) * 8U);
      block_[56U + index] = static_cast<uint8_t>((bit_length >> shift) & 0xffU);
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
  std::span<const uint8_t> pdf;
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

std::optional<uint16_t> ReadU16(std::span<const uint8_t> bytes, size_t offset) {
  if (offset > bytes.size() || bytes.size() - offset < 2U) {
    return std::nullopt;
  }
  return static_cast<uint16_t>((static_cast<uint16_t>(bytes[offset]) << 8U) |
                               static_cast<uint16_t>(bytes[offset + 1U]));
}

std::optional<uint32_t> ReadU32(std::span<const uint8_t> bytes, size_t offset) {
  if (offset > bytes.size() || bytes.size() - offset < 4U) {
    return std::nullopt;
  }
  return (static_cast<uint32_t>(bytes[offset]) << 24U) |
         (static_cast<uint32_t>(bytes[offset + 1U]) << 16U) |
         (static_cast<uint32_t>(bytes[offset + 2U]) << 8U) |
         static_cast<uint32_t>(bytes[offset + 3U]);
}

std::optional<uint64_t> ReadU64(std::span<const uint8_t> bytes, size_t offset) {
  if (offset > bytes.size() || bytes.size() - offset < 8U) {
    return std::nullopt;
  }
  uint64_t value = 0;
  for (size_t index = 0; index < 8U; ++index) {
    value = (value << 8U) | static_cast<uint64_t>(bytes[offset + index]);
  }
  return value;
}

std::optional<std::vector<uint8_t>> ReadInput() {
  std::vector<uint8_t> input;
  input.reserve(kRequestHeaderBytes + 4096U);
  std::array<uint8_t, 8192> block = {};
  while (true) {
    const size_t count = std::fread(block.data(), 1U, block.size(), stdin);
    if (count != 0U) {
      if (input.size() > kMaxRequestBytes - count) {
        return std::nullopt;
      }
      input.insert(input.end(), block.begin(),
                   block.begin() + static_cast<ptrdiff_t>(count));
    }
    if (count < block.size()) {
      if (std::ferror(stdin) != 0) {
        return std::nullopt;
      }
      break;
    }
  }
  return input;
}

std::optional<Request> ParseRequest(std::span<const uint8_t> frame) {
  if (frame.size() < kRequestHeaderBytes ||
      !std::ranges::equal(frame.first(kRequestMagic.size()), kRequestMagic)) {
    return std::nullopt;
  }
  const auto schema = ReadU16(frame, 8U);
  const auto reserved = ReadU16(frame, 10U);
  const auto page = ReadU32(frame, 12U);
  const auto width = ReadU32(frame, 16U);
  const auto height = ReadU32(frame, 20U);
  const auto pdf_size = ReadU64(frame, 24U);
  if (!schema || !reserved || !page || !width || !height || !pdf_size ||
      *schema != kSchemaVersion || *reserved != 0U || *page != 0U ||
      *width != 1U || *height != 1U || *pdf_size > kMaxPdfBytes ||
      *pdf_size != frame.size() - kRequestHeaderBytes) {
    return std::nullopt;
  }

  Request request = {};
  request.page = *page;
  request.width = *width;
  request.height = *height;
  std::ranges::copy(frame.subspan(32U, 32U), request.source_hash.begin());
  std::ranges::copy(frame.subspan(64U, 32U),
                    request.descriptor_identity.begin());
  request.pdf = frame.subspan(kRequestHeaderBytes);
  Sha256 hasher;
  hasher.Update(request.pdf);
  if (hasher.Final() != request.source_hash) {
    return std::nullopt;
  }
  return request;
}

void AppendU16(std::vector<uint8_t>* output, uint16_t value) {
  output->push_back(static_cast<uint8_t>(value >> 8U));
  output->push_back(static_cast<uint8_t>(value));
}

void AppendU32(std::vector<uint8_t>* output, uint32_t value) {
  output->push_back(static_cast<uint8_t>(value >> 24U));
  output->push_back(static_cast<uint8_t>(value >> 16U));
  output->push_back(static_cast<uint8_t>(value >> 8U));
  output->push_back(static_cast<uint8_t>(value));
}

void AppendU64(std::vector<uint8_t>* output, uint64_t value) {
  for (int shift = 56; shift >= 0; shift -= 8) {
    output->push_back(static_cast<uint8_t>(value >> shift));
  }
}

bool WriteAll(std::span<const uint8_t> bytes) {
  while (!bytes.empty()) {
    const size_t count = std::fwrite(bytes.data(), 1U, bytes.size(), stdout);
    if (count == 0U) {
      return false;
    }
    bytes = bytes.subspan(count);
  }
  return std::fflush(stdout) == 0;
}

bool WriteResponse(const Request& request, std::optional<int> page_count) {
  std::string parse_json;
  if (page_count.has_value()) {
    parse_json = "{\"schema\":1,\"page_count\":";
    parse_json.append(std::to_string(*page_count));
    parse_json.append("}\n");
  }
  const bool success = page_count.has_value();
  const size_t payload_size = success ? parse_json.size() : 0U;
  if (payload_size > kMaxJsonBytes || payload_size > UINT32_MAX) {
    return false;
  }

  std::vector<uint8_t> response;
  response.reserve(kResponseHeaderBytes + payload_size);
  response.insert(response.end(), kResponseMagic.begin(), kResponseMagic.end());
  AppendU16(&response, kSchemaVersion);
  AppendU16(&response, success ? 0U : 1U);
  if (success) {
    response.insert(response.end(), {0U, 1U, 1U, 1U});
  } else {
    response.insert(response.end(), {2U, 2U, 2U, 2U});
  }
  AppendU32(&response, static_cast<uint32_t>(payload_size));
  AppendU32(&response, 0U);
  AppendU32(&response, 0U);
  AppendU32(&response, request.page);
  AppendU32(&response, request.width);
  AppendU32(&response, request.height);
  AppendU64(&response, 0U);
  response.insert(response.end(), request.source_hash.begin(),
                  request.source_hash.end());
  response.insert(response.end(), request.descriptor_identity.begin(),
                  request.descriptor_identity.end());
  if (response.size() != kResponseHeaderBytes) {
    return false;
  }
  response.insert(response.end(), parse_json.begin(), parse_json.end());
  return WriteAll(response);
}

bool Run() {
#if defined(_WIN32)
  if (_setmode(_fileno(stdin), _O_BINARY) == -1 ||
      _setmode(_fileno(stdout), _O_BINARY) == -1) {
    return false;
  }
#endif
  static_assert(CHAR_BIT == 8);
  const auto input = ReadInput();
  if (!input) {
    return false;
  }
  const auto request = ParseRequest(*input);
  if (!request) {
    return false;
  }

  PdfiumLibrary library;
  ScopedDocument document(FPDF_LoadMemDocument64(request->pdf.data(),
                                                 request->pdf.size(), nullptr));
  if (!document.get()) {
    return WriteResponse(*request, std::nullopt);
  }
  const int page_count = FPDF_GetPageCount(document.get());
  if (page_count < 0) {
    return WriteResponse(*request, std::nullopt);
  }
  return WriteResponse(*request, page_count);
}

}  // namespace

int main(int argc, char*[]) {
  if (argc != 1 || !Run()) {
    std::fputs("RPE-PDFIUM-PAGE-COUNT-0001\n", stderr);
    return 7;
  }
  return 0;
}
