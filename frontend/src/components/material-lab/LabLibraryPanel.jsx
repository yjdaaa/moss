import React from "react";
import { Plus, Search } from "lucide-react";
import { thicknessChipLabel } from "../../materialLabState.js";
import { FoldHeader } from "../FoldHeader.jsx";

/** 材料实验室 ① 材料库：搜索、新建与材料 / 厚度表。原样搬自 pages/MaterialLabPage.jsx（2026-11-16 按职责拆分） */
export function LabLibraryPanel({
  materials,
  filteredMaterials,
  searchQuery,
  setSearchQuery,
  readOnly,
  selectedMaterial,
  selectedThickness,
  handleSelectMaterialAndThick,
  setMaterialModalData,
  setShowMaterialModal,
}) {
  return (
    <div className="hud-card ob-card lab-row-2">
      <FoldHeader id="lab-library" title="材料库" n={1} />
      <div className="lab-row-3">
        <div className="lab-box-2">
          <Search size={14} className="lab-icon-2" />
          <input
            type="text"
            placeholder="搜索材料名或别名..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="text-input lab-field-2"
          />
        </div>
        {!readOnly && (
          <button
            type="button"
            className="btn btn-hud-primary lab-btn"
            onClick={() => {
              setMaterialModalData(null);
              setShowMaterialModal(true);
            }}
            title="添加新材料到材料库"
          >
            <Plus size={13} />
            <span>新建</span>
          </button>
        )}
      </div>

      <div className="ob-tbl ob-mtbl">
        <table>
          <thead>
            <tr>
              <th>材料</th>
              <th>厚度</th>
            </tr>
          </thead>
          <tbody>
            {filteredMaterials.length === 0 ? (
              <tr>
                <td colSpan={2} className="lab-box-7">
                  {Object.keys(materials).length === 0 ? "当前材料库为空，可点击右上角新建或导入" : "无匹配材料"}
                </td>
              </tr>
            ) : (
              filteredMaterials.map((matName) => {
                const matObj = materials[matName];
                const thickKeys = Object.keys(matObj?.thicknesses || {});
                const isSelected = selectedMaterial === matName;
                return (
                  <tr
                    key={matName}
                    className={isSelected ? "on" : ""}
                    onClick={() => handleSelectMaterialAndThick(matName, thickKeys[0] || "3")}
                    aria-selected={isSelected}
                  >
                    <td>
                      <b>{matName}</b>
                      <div className="ob-sub">{thickKeys.length} 档厚度</div>
                    </td>
                    <td>
                      <div className="ob-chips">
                        {thickKeys.map((th) => (
                          <button
                            type="button"
                            key={th}
                            className={`ob-chip${isSelected && selectedThickness === th ? " on" : ""}`}
                            onClick={(event) => {
                              event.stopPropagation();
                              handleSelectMaterialAndThick(matName, th);
                            }}
                          >
                            {thicknessChipLabel(th)}
                          </button>
                        ))}
                      </div>
                    </td>
                  </tr>
                );
              })
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}
