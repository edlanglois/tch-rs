// The pre-trained weights can be downloaded here:
//   https://github.com/LaurentMazare/ocaml-torch/releases/download/v0.1-unstable/yolo-v3.ot
#[macro_use]
extern crate failure;
extern crate tch;

mod coco_classes;
mod darknet;

use tch::nn::ModuleT;
use tch::vision::image;
use tch::Tensor;
const CONFIG_NAME: &'static str = "examples/yolo/yolo-v3.cfg";
const CONFIDENCE_THRESHOLD: f64 = 0.5;
const NMS_THRESHOLD: f64 = 0.4;

#[derive(Debug, Clone, Copy)]
struct Bbox {
    xmin: f64,
    ymin: f64,
    xmax: f64,
    ymax: f64,
    confidence: f64,
    class_index: usize,
    class_confidence: f64,
}

// Intersection over union of two bounding boxes.
fn iou(b1: &Bbox, b2: &Bbox) -> f64 {
    let b1_area = (b1.xmax - b1.xmin + 1.) * (b1.ymax - b1.ymin + 1.);
    let b2_area = (b2.xmax - b2.xmin + 1.) * (b2.ymax - b2.ymin + 1.);
    let i_xmin = b1.xmin.max(b2.xmin);
    let i_xmax = b1.xmax.min(b2.xmax);
    let i_ymin = b1.ymin.max(b2.ymin);
    let i_ymax = b1.ymax.min(b2.ymax);
    let i_area = (i_xmax - i_xmin + 1.).max(0.) * (i_ymax - i_ymin + 1.).max(0.);
    i_area / (b1_area + b2_area - i_area)
}

// Assumes x1 <= x2 and y1 <= y2
pub fn draw_rect(t: &mut Tensor, x1: i64, x2: i64, y1: i64, y2: i64) {
    let color = Tensor::of_slice(&[0., 0., 1.]).view(&[3, 1, 1]);
    t.narrow(2, x1, x2 - x1)
        .narrow(1, y1, y2 - y1)
        .copy_(&color)
}

pub fn report(pred: &Tensor, img: &Tensor, w: i64, h: i64) -> failure::Fallible<()> {
    let (npreds, pred_size) = pred.size2()?;
    let nclasses = (pred_size - 5) as usize;
    // The bounding boxes grouped by (maximum) class index.
    let mut bboxes: Vec<Vec<Bbox>> = (0..nclasses).map(|_| vec![]).collect();
    // Extract the bounding boxes for which confidence is above the threshold.
    for index in 0..npreds {
        let pred = Vec::<f64>::from(pred.get(index));
        let confidence = pred[4];
        if confidence > CONFIDENCE_THRESHOLD {
            let mut class_index = 0;
            for i in 0..nclasses {
                if pred[5 + i] > pred[5 + class_index] {
                    class_index = i
                }
            }
            if pred[class_index + 5] > 0. {
                let bbox = Bbox {
                    xmin: pred[0] - pred[2] / 2.,
                    ymin: pred[1] - pred[3] / 2.,
                    xmax: pred[0] + pred[2] / 2.,
                    ymax: pred[1] + pred[3] / 2.,
                    confidence,
                    class_index,
                    class_confidence: pred[5 + class_index],
                };
                bboxes[class_index].push(bbox)
            }
        }
    }
    // Perform non-maximum suppression.
    for bboxes_for_class in bboxes.iter_mut() {
        bboxes_for_class.sort_by(|b1, b2| b2.confidence.partial_cmp(&b1.confidence).unwrap());
        let mut current_index = 0;
        for index in 0..bboxes_for_class.len() {
            let mut drop = false;
            for prev_index in 0..current_index {
                let iou = iou(&bboxes_for_class[prev_index], &bboxes_for_class[index]);
                if iou > NMS_THRESHOLD {
                    drop = true;
                    break;
                }
            }
            if !drop {
                bboxes_for_class.swap(current_index, index);
                current_index += 1;
            }
        }
        bboxes_for_class.truncate(current_index);
    }
    // Annotate the original image and print boxes information.
    let (_, initial_h, initial_w) = img.size3()?;
    let mut img = img.to_kind(tch::Kind::Float) / 255.;
    let w_ratio = initial_w as f64 / w as f64;
    let h_ratio = initial_h as f64 / h as f64;
    for (class_index, bboxes_for_class) in bboxes.iter().enumerate() {
        for b in bboxes_for_class.iter() {
            println!("{}: {:?}", coco_classes::NAMES[class_index], b);
            let xmin = ((b.xmin * w_ratio) as i64).max(0).min(initial_w - 1);
            let ymin = ((b.ymin * h_ratio) as i64).max(0).min(initial_h - 1);
            let xmax = ((b.xmax * w_ratio) as i64).max(0).min(initial_w - 1);
            let ymax = ((b.ymax * h_ratio) as i64).max(0).min(initial_h - 1);
            draw_rect(&mut img, xmin, xmax, ymin, ymax.min(ymin + 2));
            draw_rect(&mut img, xmin, xmax, ymin.max(ymax - 2), ymax);
            draw_rect(&mut img, xmin, xmax.min(xmin + 2), ymin, ymax);
            draw_rect(&mut img, xmin.max(xmax - 2), xmax, ymin, ymax);
        }
    }
    image::save(&(img * 255.).to_kind(tch::Kind::Uint8), "output.jpg")?;
    Ok(())
}

pub fn main() -> failure::Fallible<()> {
    let args: Vec<_> = std::env::args().collect();
    let (weights, image) = match args.as_slice() {
        [_, w, i] => (std::path::Path::new(w), i.to_owned()),
        _ => bail!("usage: main yolo-v3.ot image.jpg"),
    };
    // Create the model and load the weights from the file.
    let mut vs = tch::nn::VarStore::new(tch::Device::Cpu);
    let darknet = darknet::parse_config(CONFIG_NAME)?;
    let model = darknet.build_model(&vs.root())?;
    vs.load(weights)?;

    // Load the image file and resize it.
    let original_image = image::load(image)?;
    let net_width = darknet.width()?;
    let net_height = darknet.height()?;
    let image = image::resize(&original_image, net_width, net_height)?;
    let image = image.unsqueeze(0).to_kind(tch::Kind::Float) / 255.;
    let predictions = model.forward_t(&image, false).squeeze();
    report(&predictions, &original_image, net_width, net_height)?;
    Ok(())
}