# DawellService

A background service for Dawell360 that handles device registration and heartbeat communication with the server.

## Prerequisites

- **Rust** (1.70 or later) - [Install Rust](https://rustup.rs/)
- **Windows** (for Windows service functionality) or Linux/macOS

## Project Structure

```
dawellservice/
├── src/
│   ├── main.rs           # Entry point and CLI handling
│   ├── api_client.rs     # HTTP client for server communication
│   ├── config.rs         # Configuration management
│   ├── service_install.rs # OS service installation
│   └── system_info.rs    # System information collection
├── build.rs              # Build script (loads .env at compile time)
├── Cargo.toml            # Project dependencies
├── Cargo.lock            # Dependency lock file
└── .env                  # Environment configuration
```

## Environment Setup

1. Copy or create the `.env` file in the project root:

```env
# Development
DAWELLSERVICE_API_BASE_URL=http://192.168.1.49:4000/api/client

# Production (uncomment when deploying)
# DAWELLSERVICE_API_BASE_URL=https://demoapp.deskpulse.org/api/client
```

**Note:** The API URL is embedded at compile time. You must rebuild after changing the `.env` file.

## Running the Project (Development)

### 1. Build and Run

```bash
# Navigate to project directory
cd dawellservice

# Build the project
cargo build

# Run with a token (for testing installation flow)
cargo run -- --token=<BASE64_TOKEN>

# Run in service mode (after installation)
cargo run -- --run

# Uninstall
cargo run -- --uninstall
```

### 2. Run with Custom API URL

```bash
cargo run -- --token=<BASE64_TOKEN> --api_url=http://localhost:4000/api/client
```

## Building for Production

### 1. Release Build

```bash
# Standard release build
cargo build --release

# The executable will be at:
# Windows: target/release/dawellservice.exe
# Linux/macOS: target/release/dawellservice
```

### 2. Build with Specific API URL

Update the `.env` file before building:

```env
DAWELLSERVICE_API_BASE_URL=https://demoapp.deskpulse.org/api/client
```

Then rebuild:

```bash
cargo build --release
```

### 3. Cross-Compilation (Optional)

For building Windows executables on other platforms:

```bash
# Add Windows target
rustup target add x86_64-pc-windows-msvc

# Build for Windows
cargo build --release --target x86_64-pc-windows-msvc
```

## Usage

### Install the Service

Run as Administrator (Windows) or with sudo (Linux):

```bash
dawellservice --token=<BASE64_TOKEN>
```

This will:
1. Decode the token (extracts user_id and organization_id)
2. Collect system information
3. Register with the server
4. Save encrypted configuration
5. Install and start the background service

### Run in Service Mode

```bash
dawellservice --run
```

### Uninstall the Service

```bash
dawellservice --uninstall
```

This will:
1. Send offline status to server
2. Stop and remove the system service
3. Delete the configuration file

## CLI Options

| Option | Description |
|--------|-------------|
| `--token=<TOKEN>` | Base64-encoded JSON token containing user_id and organization_id |
| `--api_url=<URL>` | Override the build-time API URL |
| `--run` | Run in service mode (reads stored config, starts heartbeat loop) |
| `--uninstall` | Uninstall the service and remove config |
| `--help` | Show help information |
| `--version` | Show version |

## Service Behavior

- **Heartbeat Interval:** 30 seconds
- **Auto-recovery:** Re-registers after 3 consecutive heartbeat failures
- **Graceful shutdown:** Sends offline status when stopped
- **Logs (Windows):** Stored in config directory as `service.log`

## Troubleshooting

### Service won't start
- Ensure you have administrator/root privileges
- Check if the service is already installed
- Verify the configuration exists (run with `--token` first)

### Registration fails
- Check network connectivity
- Verify the API URL is correct
- Check server availability

### View Logs (Windows)
Logs are stored at:
```
%PROGRAMDATA%\DawellService\service.log
```

## Development Tips

### Rebuild After .env Changes
The API URL is embedded at compile time. Always rebuild after modifying `.env`:

```bash
cargo clean
cargo build
```

### Enable Debug Logging
```bash
RUST_LOG=debug cargo run -- --run
```

### Check Dependencies
```bash
cargo tree
```

## License

Proprietary - Dawell360
