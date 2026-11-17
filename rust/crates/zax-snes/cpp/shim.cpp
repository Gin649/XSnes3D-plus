// XSnes3D+: a small C interface around Mesen 2's emulator, so Rust can drive the SNES core
// one frame at a time. Only the SNES is used, but Mesen builds all of its consoles together.
//
// Every function catches C++ exceptions, so none can cross into Rust.

#include "pch.h"
#include <atomic>
#include <cstdint>
#include <memory>
#include <string>
#include "Shared/Emulator.h"
#include "Shared/EmuSettings.h"
#include "Shared/SettingTypes.h"
#include "Shared/KeyManager.h"
#include "Shared/BaseControlDevice.h"
#include "Shared/Interfaces/IConsole.h"
#include "Shared/Interfaces/IInputProvider.h"
#include "SNES/SnesConsole.h"
#include "SNES/SnesPpu.h"
#include "Utilities/FolderUtilities.h"
#include "Utilities/VirtualFile.h"

namespace {

// Hands the host's buttons to the controller in port 1 whenever the game reads it.
// The bits follow Mesen's SNES controller: A, B, X, Y, L, R, Select, Start, Up, Down,
// Left, Right in bits 0 to 11.
class HostInput : public IInputProvider
{
public:
	std::atomic<uint16_t> Buttons { 0 };

	bool SetInput(BaseControlDevice* device) override
	{
		if(device->GetPort() != 0 || device->GetControllerType() != ControllerType::SnesController) {
			return false;
		}
		uint16_t held = Buttons.load();
		for(uint8_t bit = 0; bit < 12; bit++) {
			device->SetBitValue(bit, ((held >> bit) & 1) != 0);
		}
		return true;
	}
};

} // namespace

struct XsSnes
{
	std::unique_ptr<Emulator> Emu;
	HostInput Input;
	bool Loaded = false;
};

extern "C" {

// Creates an emulator. Mesen writes its saves and settings under home_folder.
// Returns null on failure.
XsSnes* xs_create(const char* home_folder)
{
	try {
		FolderUtilities::SetHomeFolder(home_folder);
		auto snes = new XsSnes();
		snes->Emu.reset(new Emulator());
		snes->Emu->Initialize(false);
		KeyManager::SetSettings(snes->Emu->GetSettings());

		EmuSettings* settings = snes->Emu->GetSettings();

		// No frame limiter: the host decides the pace.
		EmulationConfig emulation = settings->GetEmulationConfig();
		emulation.EmulationSpeed = 0;
		emulation.RunAheadFrames = 0;
		settings->SetEmulationConfig(emulation);

		// A SNES pad in port 1, and memory that powers on as zeros so every run is the same.
		SnesConfig snesConfig = settings->GetSnesConfig();
		snesConfig.Port1.Type = ControllerType::SnesController;
		snesConfig.RamPowerOnState = RamState::AllZeros;
		snesConfig.EnableRandomPowerOnState = false;
		settings->SetSnesConfig(snesConfig);

		// Do not keep a recent-games list or show the game selection screen.
		PreferencesConfig preferences = settings->GetPreferences();
		preferences.DisableGameSelectionScreen = true;
		settings->SetPreferences(preferences);

		return snes;
	} catch(...) {
		return nullptr;
	}
}

// Loads a ROM file (.sfc, .smc, or a .zip or .7z holding one). Returns 1 on success.
int xs_load_rom(XsSnes* snes, const char* path)
{
	try {
		VirtualFile rom(path);
		VirtualFile noPatch;
		// false: do not start Mesen's own emulation thread, the host runs the frames.
		if(!snes->Emu->LoadRom(rom, noPatch, false)) {
			return 0;
		}
		if(snes->Emu->GetConsoleType() != ConsoleType::Snes) {
			return 0;
		}
		snes->Emu->RegisterInputProvider(&snes->Input);
		snes->Emu->HostBegin();
		snes->Loaded = true;
		return 1;
	} catch(...) {
		return 0;
	}
}

// Sets the port 1 buttons used from the next time the game reads the pad.
void xs_set_buttons(XsSnes* snes, uint16_t buttons)
{
	snes->Input.Buttons.store(buttons);
}

// Runs one frame. Returns 1 on success.
int xs_run_frame(XsSnes* snes)
{
	if(!snes->Loaded) {
		return 0;
	}
	try {
		snes->Emu->HostRunFrame();
		return 1;
	} catch(...) {
		return 0;
	}
}

// The last finished frame, as 15-bit BGR colours (bits 0-4 red, 5-9 green, 10-14 blue).
// The buffer is 256 wide and 239 tall, or 512 by 478 in hi-res. Without overscan the
// picture is rows 7 to 230 (14 to 461 in hi-res). The pointer stays valid until the next
// xs_run_frame. Returns null when no ROM is loaded.
const uint16_t* xs_frame(XsSnes* snes, uint32_t* width, uint32_t* height, int* overscan)
{
	if(!snes->Loaded) {
		return nullptr;
	}
	try {
		shared_ptr<IConsole> console = snes->Emu->GetConsole();
		if(!console) {
			return nullptr;
		}
		PpuFrameInfo frame = console->GetPpuFrame();
		*width = frame.Width;
		*height = frame.Height;
		*overscan = 0;
		if(auto snesConsole = dynamic_cast<SnesConsole*>(console.get())) {
			*overscan = snesConsole->GetPpu()->GetState().OverscanMode ? 1 : 0;
		}
		return reinterpret_cast<const uint16_t*>(frame.FrameBuffer);
	} catch(...) {
		return nullptr;
	}
}

// Stops the game and frees the emulator.
void xs_destroy(XsSnes* snes)
{
	if(!snes) {
		return;
	}
	try {
		if(snes->Loaded) {
			snes->Emu->UnregisterInputProvider(&snes->Input);
		}
		snes->Emu->Stop(false);
		snes->Emu->Release();
	} catch(...) {
	}
	delete snes;
}

} // extern "C"
