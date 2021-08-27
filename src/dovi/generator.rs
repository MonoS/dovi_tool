use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use crate::dovi::rpu::nits_to_pq;
use crate::dovi::OUT_NAL_HEADER;

use super::DoviRpu;

use super::rpu::{
    rpu_data_header::RpuDataHeader, vdr_dm_data::VdrDmData, vdr_rpu_data::VdrRpuData,
};

use super::CmXmlParser;

pub struct Generator {
    json_path: Option<PathBuf>,
    rpu_out: PathBuf,
    hdr10plus_path: Option<PathBuf>,
    xml_path: Option<PathBuf>,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct GenerateConfig {
    pub length: u64,
    pub target_nits: Option<u16>,

    #[serde(default)]
    pub source_min_pq: Option<u16>,

    #[serde(default)]
    pub source_max_pq: Option<u16>,

    pub level2: Option<Vec<Level2Metadata>>,
    pub level5: Option<Level5Metadata>,
    pub level6: Option<Level6Metadata>,
}

#[derive(Default, Debug, Clone)]
pub struct Level1Metadata {
    pub min_pq: u16,
    pub max_pq: u16,
    pub avg_pq: u16,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct Level2Metadata {
    pub target_nits: u16,

    #[serde(default = "default_trim")]
    pub trim_slope: u16,
    #[serde(default = "default_trim")]
    pub trim_offset: u16,
    #[serde(default = "default_trim")]
    pub trim_power: u16,
    #[serde(default = "default_trim")]
    pub trim_chroma_weight: u16,
    #[serde(default = "default_trim")]
    pub trim_saturation_gain: u16,
    #[serde(default = "default_trim_neg")]
    pub ms_weight: i16,
}

#[derive(Default, Debug, Clone)]
pub struct Level3Metadata {
    pub min_pq_offset: u16,
    pub max_pq_offset: u16,
    pub avg_pq_offset: u16,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Level5Metadata {
    pub active_area_left_offset: u16,
    pub active_area_right_offset: u16,
    pub active_area_top_offset: u16,
    pub active_area_bottom_offset: u16,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct Level6Metadata {
    pub max_display_mastering_luminance: u16,
    pub min_display_mastering_luminance: u16,
    pub max_content_light_level: u16,
    pub max_frame_average_light_level: u16,
}

impl Generator {
    pub fn generate(
        json_path: Option<PathBuf>,
        rpu_out: Option<PathBuf>,
        hdr10plus_path: Option<PathBuf>,
        xml_path: Option<PathBuf>,
    ) {
        let out_path = if let Some(out_path) = rpu_out {
            out_path
        } else {
            PathBuf::from("RPU_generated.bin".to_string())
        };

        let generator = Generator {
            json_path,
            rpu_out: out_path,
            hdr10plus_path,
            xml_path,
        };

        println!("Generating metadata...");

        if let Some(json_path) = &generator.json_path {
            let json_file = File::open(json_path).unwrap();
            let config: GenerateConfig = serde_json::from_reader(&json_file).unwrap();

            println!("{:#?}", config);

            if let Err(res) = generator.execute(&config) {
                panic!("{:?}", res);
            }
        } else if let Some(xml_path) = &generator.xml_path {
            if let Err(res) = generator.generate_from_xml(xml_path) {
                panic!("{:?}", res);
            }
        }

        println!("Done.")
    }

    fn execute(&self, config: &GenerateConfig) -> Result<(), std::io::Error> {
        let (l1_meta, scene_cuts) = parse_hdr10plus_for_l1(&self.hdr10plus_path);

        let mut writer = BufWriter::with_capacity(
            100_000,
            File::create(&self.rpu_out).expect("Can't create file"),
        );

        let length = if let Some(l1) = &l1_meta {
            l1.len()
        } else {
            config.length as usize
        };

        for i in 0..length {
            let mut rpu = DoviRpu {
                dovi_profile: 8,
                modified: true,
                header: RpuDataHeader::p8_default(),
                vdr_rpu_data: Some(VdrRpuData::p8_default()),
                nlq_data: None,
                vdr_dm_data: Some(VdrDmData::from_config(config)),
                last_byte: 0x80,
                ..Default::default()
            };

            let encoded_rpu = if let Some(l1_list) = &l1_meta {
                if let Some(meta) = &l1_list.get(i) {
                    if let Some(dm_meta) = &mut rpu.vdr_dm_data {
                        dm_meta.add_level1_metadata(meta.min_pq, meta.max_pq, meta.avg_pq);

                        if scene_cuts.contains(&i) {
                            dm_meta.set_scene_cut(true);
                        }
                    }
                }

                rpu.write_rpu_data()
            } else {
                rpu.write_rpu_data()
            };

            writer.write_all(OUT_NAL_HEADER)?;

            // Remove 0x7C01
            writer.write_all(&encoded_rpu[2..])?;
        }

        println!("Generated metadata for {} frames", length);

        writer.flush()?;

        Ok(())
    }

    fn generate_from_xml(&self, xml_path: &Path) -> Result<(), std::io::Error> {
        let mut s = String::new();
        File::open(xml_path)
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();

        let parser = CmXmlParser::new(s);

        let length = parser.get_video_length();
        let level6 = parser.get_hdr10_metadata();

        let config = GenerateConfig {
            length: 0,
            level6: Some(level6.clone()),
            ..Default::default()
        };

        let mut writer = BufWriter::with_capacity(
            100_000,
            File::create(&self.rpu_out).expect("Can't create file"),
        );

        let shots = parser.get_shots();

        for shot in shots {
            let end = shot.duration;

            for i in 0..end {
                let mut rpu = DoviRpu {
                    dovi_profile: 8,
                    modified: true,
                    header: RpuDataHeader::p8_default(),
                    vdr_rpu_data: Some(VdrRpuData::p8_default()),
                    nlq_data: None,
                    vdr_dm_data: Some(VdrDmData::from_config(&config)),
                    last_byte: 0x80,
                    ..Default::default()
                };

                if let Some(dm_meta) = &mut rpu.vdr_dm_data {
                    if let Some(l1_list) = &shot.level1 {
                        if let Some(meta) = l1_list.get(i) {
                            dm_meta.add_level1_metadata(meta.min_pq, meta.max_pq, meta.avg_pq);

                            if i == 0 {
                                dm_meta.set_scene_cut(true);
                            }
                        }
                    }

                    if let Some(l2_list) = &shot.level2 {
                        if let Some(meta) = l2_list.get(i) {
                            for l2 in meta {
                                dm_meta.add_level2_metadata(
                                    l2.target_nits,
                                    l2.trim_slope,
                                    l2.trim_offset,
                                    l2.trim_power,
                                    l2.trim_chroma_weight,
                                    l2.trim_saturation_gain,
                                    l2.ms_weight,
                                )
                            }
                        }
                    }

                    if let Some(l3_list) = &shot.level3 {
                        if let Some(meta) = l3_list.get(i) {
                            dm_meta.add_level3_metadata(
                                meta.min_pq_offset,
                                meta.max_pq_offset,
                                meta.avg_pq_offset,
                            );
                        }
                    }
                }

                let encoded_rpu = rpu.write_rpu_data();

                writer.write_all(OUT_NAL_HEADER)?;

                // Remove 0x7C01
                writer.write_all(&encoded_rpu[2..])?;
            }
        }

        println!("Generated metadata for {} frames", length);

        writer.flush()?;

        Ok(())
    }
}

fn parse_hdr10plus_for_l1(
    hdr10plus_path: &Option<PathBuf>,
) -> (Option<Vec<Level1Metadata>>, Vec<usize>) {
    let mut l1_meta = None;
    let mut scene_cuts: Vec<usize> = Vec::new();

    if let Some(path) = hdr10plus_path {
        let mut s = String::new();
        File::open(path).unwrap().read_to_string(&mut s).unwrap();

        let hdr10plus: Value = serde_json::from_str(&s).unwrap();

        if let Some(json) = hdr10plus.as_object() {
            if let Some(scene_info) = json.get("SceneInfo") {
                if let Some(list) = scene_info.as_array() {
                    let info_list = list
                        .iter()
                        .filter_map(|e| e.as_object())
                        .map(|e| {
                            let lum_v = e.get("LuminanceParameters").unwrap();
                            let lum = lum_v.as_object().unwrap();

                            let avg_rgb = lum.get("AverageRGB").unwrap().as_u64().unwrap();
                            let maxscl = lum.get("MaxScl").unwrap().as_array().unwrap();

                            let max_rgb = maxscl.iter().filter_map(|e| e.as_u64()).max().unwrap();

                            let scene_frame_index =
                                e.get("SceneFrameIndex").unwrap().as_u64().unwrap() as usize;

                            if scene_frame_index == 0 {
                                let sequence_frame_index =
                                    e.get("SequenceFrameIndex").unwrap().as_u64().unwrap() as usize;

                                scene_cuts.push(sequence_frame_index);
                            }

                            Level1Metadata {
                                min_pq: 0,
                                max_pq: (nits_to_pq((max_rgb as f64 / 10.0).round() as u16)
                                    * 4095.0)
                                    .round() as u16,
                                avg_pq: (nits_to_pq((avg_rgb as f64 / 10.0).round() as u16)
                                    * 4095.0)
                                    .round() as u16,
                            }
                        })
                        .collect();

                    l1_meta = Some(info_list)
                }
            }
        }
    }

    (l1_meta, scene_cuts)
}

fn default_trim() -> u16 {
    2048
}

fn default_trim_neg() -> i16 {
    2048
}
