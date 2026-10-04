# Chạy thử fork riêng trên Mac

Bản bootstrap giữ tên ChatCMD và protocol hiện có. Updater chỉ tìm release của
`sonnguyen8704/ChatCmd`, kiểm tra URL theo đúng fork, tag và asset. Khi fork chưa có
release, HTTP 404 được xử lý là chưa có bản cập nhật. Tên ZIP giữ nguyên để tương
thích workflow đóng gói hiện có. Chưa đổi bundle ID, icon hoặc data namespace của
bản cài mặc định; chưa nên cài đè bản upstream.

## Candidate trên Mac Apple Silicon

Cần Git, Node 20.19+/22.12+ và Rust 1.85+ cùng Xcode Command Line Tools.
Clone fork, checkout branch `feat/fork-bootstrap`, rồi build tại checkout riêng:

```bash
cd web
npm ci
npm run build
cd ..
cargo build --features embedded-web
bash scripts/run-candidate.sh
```

Mở http://127.0.0.1:8081. Launcher chỉ chạy debug binary (self-update installation
bị vô hiệu hóa trong debug), bind loopback, ghi database/log vào
`.smoke/candidate/` trong checkout và không kế thừa DB/port/log của stable.
Mỗi checkout dùng dữ liệu riêng. Launcher không copy dữ liệu stable, không đổi
binary stable và từ chối symlink ở các vị trí dữ liệu chính.

Nếu port 8081 đang dùng, dừng candidate trước đó; không chạy hai instance trên cùng
candidate DB. Đây là cách bố trí vận hành, không phải sandbox OS hoặc bảo đảm chống
race symlink. Agent được quyền shell rộng vẫn có thể thao tác ngoài workspace.

## Smoke test cần chạy trên máy Mac

1. Giữ stable ở 8080; khởi động candidate ở 8081.
2. Xác nhận candidate là dữ liệu mới, không có task/profile của stable.
3. Tạo project test và profile đọc file; kiểm tra MCP và task đọc một file.
4. Kiểm tra terminal, cancellation và restart bằng dữ liệu test.
5. Kiểm tra updater: chưa có release không crash; không tải upstream.
6. Dừng candidate; stable phải vẫn sử dụng được.

Extension/tunnel/MCP URL cần cấu hình riêng cho candidate; launcher không tự thay
endpoint của client. Chưa xác nhận extension hoạt động ở 8081 trên browser thật.

## Kiểm tra tự động

```bash
python3 scripts/test_candidate.py
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Không phát hành/cài candidate làm stable trước khi Mac smoke và các kiểm tra Rust
pass. Bước tiếp theo: bundle/data identity riêng, đóng gói Apple Silicon và luồng
nâng cấp/rollback có snapshot SQLite nhất quán.
